// SPDX-License-Identifier: AGPL-3.0-only

//! Visual charter — the pastille that never lies.
//!
//! This module maps a fleet row onto a single [`RowKind`]: a glyph + color
//! pair that conveys the row's operational state at one glance. The taxonomy
//! deliberately replaces the ambiguous word *stalled*, which confused three
//! unrelated situations (parked on purpose, blocked on a graph edge, worker
//! genuinely gone) under the same alarming label.
//!
//! # Orthogonality — one info, one column
//!
//! The fleet tables in `cs peek` and `cs ensemble` carry **two independent
//! axes** side by side:
//!
//! - **♥ (MH) column — LIFECYCLE.** Where the molecule is in its own life:
//!   running, pending, blocked, frozen, terminal, ghost, drift. Computed
//!   here via [`classify`]; depends only on the status's
//!   [`Phase`], `heartbeat`, `has_blockers`,
//!   and the structural alarm flags `ghost`/`drift`. `classify` reads the
//!   phase rather than re-deciding the status alphabet for itself — it is
//!   one of six sites that each used to own a private classification of
//!   the same domain, and disagree with the other five.
//! - **T column — PRIORITY.** What the operator has decided about it:
//!   `temp:hot`, `temp:warm`, `temp:cold`, `temp:frozen`. Rendered by
//!   `temp_token()` in `peek_tui`; read straight from the tag set.
//!
//! The two columns must **never share a signal**. A molecule tagged
//! `temp:hot` shows 🔥 in T; its ♥ column shows whatever the lifecycle
//! actually is (idle, blocked, …) — not another 🔥. This is the charter
//! restoration of the charter (*"La flamme qui doublait"*).
//! See ADR-052 §`RowKind` charter.
//!
//! # Legend
//!
//! | Glyph | Name     | Color  | Meaning |
//! |-------|----------|--------|---------|
//! | `♥`   | healthy  | green  | running with a live worker — the happy path |
//! | `·`   | blocked  | orange | waiting on something it does not control: an unresolved upstream blocker, or a `starved` quota (ADR-062) |
//! | `🧊`  | frozen   | cyan   | frozen status — dormant by design |
//! | `👻`  | ghost    | red    | running but worker gone (orphaned / diverged) |
//! | `⚠`   | drift    | red    | structural incoherence (e.g. frozen + live worker) |
//! | `◌`   | terminal | dim    | completed / collapsed — below the fold |
//! | `💤`  | idle     | dim    | pending, no blocker — the molecule is literally dormant in its lifecycle, awaiting triage |
//!
//! # Color discipline
//!
//! Red is reserved for situations that need operator action *now*
//! (ghost, drift). Orange signals waiting on a dependency (blocked).
//! Green means a worker is alive and making progress. Cyan is frozen
//! by design. Dim is terminal or idle. The priority palette (yellow /
//! orange for parked / hot) lives in the T column, not here.
//!
//! # Machine-readable output
//!
//! This module is plaintext-only. `cs ensemble --json` and other
//! machine-readable surfaces keep reading `status`, `tags`, and
//! `effective` — not emojis. That means the glyph layer can evolve
//! without breaking any script.
//!
//! See ADR-052 §Invariants I3/I4/I5 for the states being visualised,
//! and the chronicles "La pastille qui ne ment plus" (2026-04-19) and
//! "La flamme qui doublait" (2026-04-19, sibling fix).

use colored::{ColoredString, Colorize};
use cosmon_core::molecule::{MoleculeStatus, Phase};
use cosmon_observability::HeartbeatTier;

/// The visual classification of a fleet row.
///
/// Computed fresh per render from molecule status, worker heartbeat,
/// temperature tag, and blocker count. Never persisted — this is display
/// logic, not domain state. See module docs for the full legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// Running with a live worker heartbeat (Active / Idle / Quiet).
    Healthy,
    /// **Deprecated charter variant.** Was emitted for pending molecules
    /// tagged `temp:warm` / `temp:cold`. No longer produced by
    /// [`classify`] — priority tags live in the T column, not the
    /// lifecycle column (orthogonality fix). Kept
    /// in the enum so downstream callers and old tests still compile;
    /// remove once the grace window closes.
    #[allow(dead_code)]
    Parked,
    /// **Deprecated charter variant.** Was emitted for pending molecules
    /// tagged `temp:hot`. No longer produced by [`classify`] — the T
    /// column's 🔥 already conveys `temp:hot`; repeating it in the
    /// lifecycle column turned two columns into one signal
    /// (chronicle *"La flamme qui doublait"*).
    /// Kept in the enum for backward compat; remove once the grace
    /// window closes.
    #[allow(dead_code)]
    Hot,
    /// The molecule is waiting on something it does not control: an
    /// unresolved `BlockedBy` predecessor, or an external authority
    /// refusing service (`starved`, ADR-062).
    ///
    /// The two arrive by different routes and read the same to the
    /// operator, because the gesture is the same one: you cannot re-prompt
    /// your way out of either. `starved` used to land in [`Idle`](Self::Idle)
    /// — "dormant, awaiting triage" — which inverted its meaning. It is the
    /// one status whose entire purpose is to summon someone.
    Blocked,
    /// Frozen status — dormant by design.
    Frozen,
    /// Running but the worker is Orphaned (tmux session gone) —
    /// the only truly alarming state on the happy-path axis.
    Ghost,
    /// Structural incoherence the operator must resolve manually:
    /// frozen + live worker, completed + unmerged branch, etc.
    Drift,
    /// Terminal (completed / collapsed) — below the fold.
    Terminal,
    /// Pending, unblocked — the molecule is dormant in its own lifecycle
    /// and awaiting triage. This is a **lifecycle** signal (the cycle itself
    /// is at rest), **not** a priority signal: tags like `temp:warm` live
    /// in the T column and do not influence this variant. Every pending
    /// molecule with no resolved blocker and no live worker is `Idle`,
    /// regardless of whether the operator parked it on purpose.
    ///
    /// Regression history: a later fix restored the 💤 glyph on
    /// `Idle` after the orthogonality fix accidentally
    /// retired the *lifecycle-dormant* signal along with the *priority-parked*
    /// signal it targeted. The two meanings of 💤 were never the same: one
    /// was a cycle state, the other an operator intent. The cycle state
    /// stays; the priority intent moved to the T column.
    Idle,
}

impl RowKind {
    /// Single visible glyph used in fleet tables.
    ///
    /// The glyphs are chosen so the operator can read the column at a
    /// glance without reading text: 💤 means dormant-in-lifecycle (a
    /// pending molecule at rest in its own cycle), 👻 means ghost,
    /// 🧊 means frozen. Monospace-safe widths are respected by
    /// `crate::cmd::peek_tui::pad_to_visible_width` downstream.
    ///
    /// Note: the deprecated `Parked` / `Hot` variants still carry
    /// transitional glyphs for back-compat but are never produced by
    /// [`classify`]; see their doc comments.
    #[must_use]
    // The deprecated `Parked` variant intentionally shares the 💤 glyph
    // with the active `Idle` variant during the grace window: `classify`
    // no longer produces `Parked`, but historical callers matching on
    // it still render the same dormant pictogram they used to. Once the
    // grace window closes and the variant is removed, this allow goes
    // with it.
    #[allow(clippy::match_same_arms)]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Healthy => "♥",
            // Deprecated: no longer produced by classify (see variant
            // docs). Keeps its historical 💤 for back-compat; the
            // *lifecycle-dormant* 💤 signal now lives on Idle, which is
            // what `classify()` actually returns.
            Self::Parked => "💤",
            // Deprecated: no longer produced by classify.
            Self::Hot => "🔥",
            Self::Blocked => "·",
            Self::Frozen => "🧊",
            Self::Ghost => "👻",
            Self::Drift => "⚠",
            Self::Terminal => "◌",
            Self::Idle => "💤",
        }
    }

    /// Short human label. Deliberately does **not** include the word
    /// *stalled* — that term was ambiguous by design and has been
    /// retired from the visual vocabulary.
    #[must_use]
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Parked => "parked",
            Self::Hot => "hot",
            Self::Blocked => "blocked",
            Self::Frozen => "frozen",
            Self::Ghost => "ghost",
            Self::Drift => "drift",
            Self::Terminal => "terminal",
            Self::Idle => "idle",
        }
    }

    /// Colorize a string with the kind's human-output color for the
    /// `colored` crate (used by `cs ensemble`).
    ///
    /// Orange is approximated via truecolor because it is missing from
    /// the ANSI-16 palette; terminals that fall back to 16-color mode
    /// will render it as yellow, which is still inside the "pending,
    /// needs attention" band.
    #[must_use]
    pub fn colorize(self, s: &str) -> ColoredString {
        match self {
            Self::Healthy => s.green().bold(),
            Self::Parked => s.yellow(),
            Self::Hot => s.truecolor(255, 165, 0).bold(),
            Self::Blocked => s.truecolor(255, 165, 0),
            Self::Frozen => s.cyan(),
            Self::Ghost | Self::Drift => s.red().bold(),
            Self::Terminal | Self::Idle => s.dimmed(),
        }
    }

    /// Ratatui style used by the `cs peek` TUI.
    #[must_use]
    pub fn ratatui_style(self) -> ratatui::style::Style {
        use ratatui::style::{Color, Modifier, Style};
        match self {
            Self::Healthy => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            Self::Parked => Style::default().fg(Color::Yellow),
            // Orange is not an ANSI-16 name; LightRed is the closest
            // themeable approximation and stays legible under gruvbox /
            // solarized mappings (delib-b8c6 P1/C2).
            Self::Hot => Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
            Self::Blocked => Style::default().fg(Color::LightRed),
            Self::Frozen => Style::default().fg(Color::Cyan),
            Self::Ghost | Self::Drift => {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            }
            Self::Terminal | Self::Idle => Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        }
    }

    /// `true` when this row wants the operator's eye *now* — ghosts and
    /// drift only. Hot backlog is visible (orange) but not "alarming":
    /// it's a parked candidate, not a malfunction.
    #[must_use]
    #[allow(dead_code)]
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::Ghost | Self::Drift)
    }
}

/// Whisper indicator. Returns the 🫧 glyph when the molecule has at least
/// one fresh `whispers.jsonl` entry, otherwise a blank placeholder so
/// rows stay column-aligned. The 1-bit annotation means "this molecule
/// was touched by a human hand."
#[must_use]
pub fn whisper_token(fresh: bool) -> (&'static str, ratatui::style::Style) {
    use ratatui::style::{Color, Style};
    if fresh {
        ("🫧", Style::default().fg(Color::Cyan))
    } else {
        (" ", Style::default())
    }
}

/// Temperature tag → semantic glyph. Returns a blank placeholder when the
/// molecule has no `temp:*` tag so rows stay column-aligned. This is the
/// T-column renderer (priority), orthogonal to [`RowKind`] (lifecycle).
#[must_use]
pub fn temp_token(tags: &[String]) -> (&'static str, ratatui::style::Style) {
    use ratatui::style::{Color, Style};
    for t in tags {
        match t.as_str() {
            "temp:hot" => return ("🔥", Style::default().fg(Color::Red)),
            "temp:warm" => return ("🌡", Style::default().fg(Color::Yellow)),
            "temp:cold" => return ("❄", Style::default().fg(Color::Cyan)),
            "temp:frozen" => return ("🧊", Style::default().fg(Color::Blue)),
            _ => {}
        }
    }
    (" ", Style::default())
}

/// Inputs to the visual classifier. Grouped so callers can build it from
/// either a `peek` row or an `ensemble` worker row without inventing a
/// cross-crate shared type.
#[derive(Debug, Clone)]
pub struct RowInputs<'a> {
    /// Molecule lifecycle status (`pending`, `running`, `frozen`, …).
    pub status: MoleculeStatus,
    /// Worker heartbeat, when a worker is attached to the row. `None`
    /// when the molecule has no worker (pending, frozen).
    pub heartbeat: Option<HeartbeatTier>,
    /// The molecule's tag set. **Intentionally ignored by [`classify`]**
    /// — priority tags (`temp:*`) belong to
    /// the T column, not the lifecycle column. The field is preserved
    /// in the struct so callers keep their ergonomics, and the runtime
    /// test `visual_row_kind_is_lifecycle_only` guards against anyone
    /// re-introducing tag-based branches in the classifier.
    #[allow(dead_code)]
    pub tags: &'a [String],
    /// `true` when at least one `BlockedBy` predecessor is still
    /// non-terminal.
    pub has_blockers: bool,
    /// `true` when the molecule carries a ghost signal from
    /// [`cosmon_core::run_state::RunState::ghost`] — the caller has
    /// already projected the run-state and concluded the row is a
    /// ghost.
    pub ghost: bool,
    /// `true` when the row is internally inconsistent outside of the
    /// ghost taxonomy (e.g. frozen but with a live worker). Forwards
    /// to [`RowKind::Drift`].
    pub drift: bool,
}

/// Classify a row into its visual kind.
///
/// **Axis discipline.** The ♥ column shows *lifecycle* — where the
/// molecule sits in its own cycle (running / blocked / frozen / …).
/// The T column shows *priority* — what the operator decided
/// (`temp:hot`, `temp:warm`, …). Classify reads the lifecycle inputs
/// only; it deliberately ignores the `temp:*` tag set so the two
/// columns stay orthogonal and never repeat a signal.
///
/// The order of precedence is:
///
/// 1. [`RowKind::Drift`] / [`RowKind::Ghost`] — structural alarms first.
/// 2. [`Phase::Failed`] / [`Phase::Done`] → [`RowKind::Terminal`].
/// 3. [`Phase::Parked`] → [`RowKind::Frozen`] — dormant by design.
/// 4. [`Phase::Live`] → [`RowKind::Healthy`], or `Ghost` if the session died.
/// 5. [`Phase::Blocked`] → [`RowKind::Blocked`] — ADR-062.
/// 6. [`Phase::Waiting`]: [`RowKind::Blocked`] → [`RowKind::Idle`].
///
/// The status is read through [`MoleculeStatus::phase`] and the match has
/// no wildcard: this function used to end in `_ => {}`, which is how
/// `starved` — a molecule an external authority is actively refusing to
/// serve — came out the far end wearing the 💤 of a row nobody has
/// triaged yet.
#[must_use]
pub fn classify(input: &RowInputs<'_>) -> RowKind {
    if input.drift {
        return RowKind::Drift;
    }
    match input.status.phase() {
        // Terminal and parked rows outrank the ghost signal: a frozen
        // molecule has no session by design, and a completed one has
        // nothing left to be a ghost of.
        Phase::Failed | Phase::Done => RowKind::Terminal,
        Phase::Parked => RowKind::Frozen,
        Phase::Live | Phase::Blocked | Phase::Waiting if input.ghost => RowKind::Ghost,
        Phase::Live => match input.heartbeat {
            Some(HeartbeatTier::Orphaned) => RowKind::Ghost,
            Some(_) | None => RowKind::Healthy,
        },
        // An external authority is refusing service. Rotate or wait —
        // never a re-prompt, and never a 💤.
        Phase::Blocked => RowKind::Blocked,
        // Waiting. Priority tags (temp:*) are intentionally not consulted
        // here — they belong to the T column.
        Phase::Waiting if input.has_blockers => RowKind::Blocked,
        Phase::Waiting => RowKind::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(status: MoleculeStatus) -> RowInputs<'static> {
        RowInputs {
            status,
            heartbeat: None,
            tags: &[],
            has_blockers: false,
            ghost: false,
            drift: false,
        }
    }

    /// ADR-062: `starved` means an external authority refused service. The
    /// molecule is alive and the repair is a wait or a rotation.
    ///
    /// It used to fall through this classifier's `_ =>` arm and come out as
    /// `Idle` — the 💤 of a molecule nobody has triaged yet — which is the
    /// exact inverse of what it means. The pastille that never lies was
    /// lying about the one row that most needed to be believed.
    #[test]
    fn classifies_starved_as_blocked_never_idle() {
        assert_eq!(classify(&inputs(MoleculeStatus::Starved)), RowKind::Blocked);
    }

    #[test]
    fn classifies_queued_like_pending() {
        // Two facts, one phase, one pastille.
        assert_eq!(
            classify(&inputs(MoleculeStatus::Queued)),
            classify(&inputs(MoleculeStatus::Pending)),
        );
    }

    #[test]
    fn classifies_running_with_active_heartbeat_as_healthy() {
        let hb = Some(HeartbeatTier::Active);
        let mut i = inputs(MoleculeStatus::Running);
        i.heartbeat = hb;
        assert_eq!(classify(&i), RowKind::Healthy);
    }

    #[test]
    fn classifies_running_with_idle_heartbeat_as_healthy() {
        let mut i = inputs(MoleculeStatus::Running);
        i.heartbeat = Some(HeartbeatTier::Idle);
        assert_eq!(classify(&i), RowKind::Healthy);
    }

    #[test]
    fn classifies_running_with_stalled_heartbeat_as_healthy() {
        // "stalled" heartbeat used to repaint red, which lied about the
        // problem: a live tmux with no output for 30+ minutes is an
        // *idle* worker, not a dead one. Keep green ♥ until the tmux
        // actually disappears (Orphaned).
        let mut i = inputs(MoleculeStatus::Running);
        i.heartbeat = Some(HeartbeatTier::Stalled);
        assert_eq!(classify(&i), RowKind::Healthy);
    }

    #[test]
    fn classifies_running_with_orphaned_heartbeat_as_ghost() {
        let mut i = inputs(MoleculeStatus::Running);
        i.heartbeat = Some(HeartbeatTier::Orphaned);
        assert_eq!(classify(&i), RowKind::Ghost);
    }

    /// Regression for *"La flamme qui doublait"*.
    /// Reproduces exactly the operator-observed case (pending + temp:hot,
    /// no blockers, no worker) and asserts the ♥ column stays idle.
    /// Before the fix, this input produced `RowKind::Hot` → 🔥, which
    /// duplicated the 🔥 already displayed in the T column.
    #[test]
    fn visual_hot_tag_does_not_leak_to_row_kind() {
        let tags = ["temp:hot".to_owned()];
        let mut i = inputs(MoleculeStatus::Pending);
        i.tags = &tags;
        assert_eq!(
            classify(&i),
            RowKind::Idle,
            "pending + temp:hot with no blockers must be Idle (lifecycle), \
             not Hot (priority) — orthogonality charter",
        );
    }

    #[test]
    fn classifies_pending_with_temp_hot_and_blockers_as_blocked() {
        let tags = ["temp:hot".to_owned()];
        let mut i = inputs(MoleculeStatus::Pending);
        i.tags = &tags;
        i.has_blockers = true;
        assert_eq!(
            classify(&i),
            RowKind::Blocked,
            "temp:hot does not override the lifecycle signal — a blocked \
             pending stays blocked in the ♥ column",
        );
    }

    #[test]
    fn classifies_pending_with_temp_warm_as_idle() {
        let tags = ["temp:warm".to_owned()];
        let mut i = inputs(MoleculeStatus::Pending);
        i.tags = &tags;
        assert_eq!(classify(&i), RowKind::Idle);
    }

    #[test]
    fn classifies_pending_with_temp_cold_as_idle() {
        let tags = ["temp:cold".to_owned()];
        let mut i = inputs(MoleculeStatus::Pending);
        i.tags = &tags;
        assert_eq!(classify(&i), RowKind::Idle);
    }

    #[test]
    fn classifies_pending_with_blockers_as_blocked() {
        let mut i = inputs(MoleculeStatus::Pending);
        i.has_blockers = true;
        assert_eq!(classify(&i), RowKind::Blocked);
    }

    #[test]
    fn classifies_pending_untagged_unblocked_as_idle() {
        assert_eq!(classify(&inputs(MoleculeStatus::Pending)), RowKind::Idle);
    }

    /// Charter invariant: `classify()` must depend
    /// **only** on lifecycle inputs — `status`, `heartbeat`,
    /// `has_blockers`, `ghost`, `drift`. No `temp:*` tag may shift the
    /// `RowKind`. This guards against reintroducing the duplicated-column
    /// bug under a different tag name.
    #[test]
    fn visual_row_kind_is_lifecycle_only() {
        let temp_tags = [
            "temp:hot".to_owned(),
            "temp:warm".to_owned(),
            "temp:cold".to_owned(),
            "temp:frozen".to_owned(),
        ];
        let statuses = [
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
            MoleculeStatus::Running,
            MoleculeStatus::Frozen,
            MoleculeStatus::Completed,
            MoleculeStatus::Collapsed,
        ];
        let heartbeats = [
            None,
            Some(HeartbeatTier::Active),
            Some(HeartbeatTier::Idle),
            Some(HeartbeatTier::Quiet),
            Some(HeartbeatTier::Stalled),
            Some(HeartbeatTier::Orphaned),
        ];
        for status in statuses {
            for hb in heartbeats {
                for has_blockers in [false, true] {
                    let baseline = RowInputs {
                        status,
                        heartbeat: hb,
                        tags: &[],
                        has_blockers,
                        ghost: false,
                        drift: false,
                    };
                    let expected = classify(&baseline);
                    for tag in &temp_tags {
                        let tag_slice = std::slice::from_ref(tag);
                        let with_tag = RowInputs {
                            status,
                            heartbeat: hb,
                            tags: tag_slice,
                            has_blockers,
                            ghost: false,
                            drift: false,
                        };
                        assert_eq!(
                            classify(&with_tag),
                            expected,
                            "tag {tag:?} must not shift RowKind \
                             (status={status:?}, heartbeat={hb:?}, \
                             has_blockers={has_blockers})",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn frozen_status_classifies_as_frozen_regardless_of_worker() {
        let mut i = inputs(MoleculeStatus::Frozen);
        i.heartbeat = Some(HeartbeatTier::Active);
        assert_eq!(classify(&i), RowKind::Frozen);
    }

    #[test]
    fn completed_classifies_as_terminal() {
        assert_eq!(
            classify(&inputs(MoleculeStatus::Completed)),
            RowKind::Terminal
        );
    }

    #[test]
    fn collapsed_classifies_as_terminal() {
        assert_eq!(
            classify(&inputs(MoleculeStatus::Collapsed)),
            RowKind::Terminal
        );
    }

    #[test]
    fn drift_flag_overrides_every_other_axis() {
        // Frozen + live worker + temp:hot — drift still wins because
        // internal incoherence takes priority over any other signal.
        let tags = ["temp:hot".to_owned()];
        let i = RowInputs {
            status: MoleculeStatus::Frozen,
            heartbeat: Some(HeartbeatTier::Active),
            tags: &tags,
            has_blockers: true,
            ghost: true,
            drift: true,
        };
        assert_eq!(classify(&i), RowKind::Drift);
    }

    #[test]
    fn ghost_flag_upgrades_running_even_with_fresh_heartbeat() {
        // A run-state ghost (e.g. DeadPane — pilot intent Run but the
        // witness said Dead) must surface even when the heartbeat
        // classifier hasn't caught up yet.
        let mut i = inputs(MoleculeStatus::Running);
        i.heartbeat = Some(HeartbeatTier::Active);
        i.ghost = true;
        assert_eq!(classify(&i), RowKind::Ghost);
    }

    // ── Charter tests ──────────────────────────────────────────

    /// Every **active** [`RowKind`] variant has a distinct glyph so the
    /// column can be read without consulting the color channel.
    ///
    /// Deprecated variants (`Parked`, `Hot`) are excluded because they
    /// are never produced by [`classify`] — their glyphs only matter for
    /// back-compat on external matchers and are allowed to alias active
    /// variants (specifically, `Parked` keeps 💤, which is now also the
    /// active glyph for `Idle`).
    #[test]
    fn glyphs_are_distinct() {
        let active = [
            RowKind::Healthy,
            RowKind::Blocked,
            RowKind::Frozen,
            RowKind::Ghost,
            RowKind::Drift,
            RowKind::Terminal,
            RowKind::Idle,
        ];
        let mut glyphs: Vec<&str> = active.iter().map(|k| k.glyph()).collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), active.len());
    }

    /// Anti-regression for *"Le Zzz de lifecycle"*.
    /// The 💤 glyph carried two meanings historically: (1) lifecycle-dormant
    /// (a pending molecule at rest in its own cycle) and (2) priority-parked
    /// (temp:warm). The orthogonality fix correctly
    /// retired meaning (2) from the ♥ column, but collaterally wiped
    /// meaning (1) — `Idle.glyph()` was left as a blank space, making the
    /// column *invisible* for the most common row kind. This test pins
    /// the lifecycle-dormant signal to 💤 so it cannot silently disappear
    /// again.
    #[test]
    fn visual_idle_glyph_is_zzz() {
        assert_eq!(
            RowKind::Idle.glyph(),
            "💤",
            "Idle is the lifecycle-dormant row (pending, no blocker, no \
             worker) and must render 💤 in the ♥ column; a blank glyph \
             makes every pending molecule invisible",
        );
    }

    /// Regression: no label contains the retired word *stalled*.
    #[test]
    fn no_label_contains_stalled() {
        let all = [
            RowKind::Healthy,
            RowKind::Parked,
            RowKind::Hot,
            RowKind::Blocked,
            RowKind::Frozen,
            RowKind::Ghost,
            RowKind::Drift,
            RowKind::Terminal,
            RowKind::Idle,
        ];
        for kind in all {
            assert!(
                !kind.label().to_lowercase().contains("stall"),
                "label for {kind:?} must not contain `stall`: {:?}",
                kind.label(),
            );
        }
    }

    #[test]
    fn only_ghost_and_drift_need_attention() {
        let noisy = [RowKind::Ghost, RowKind::Drift];
        let quiet = [
            RowKind::Healthy,
            RowKind::Parked,
            RowKind::Hot,
            RowKind::Blocked,
            RowKind::Frozen,
            RowKind::Terminal,
            RowKind::Idle,
        ];
        for k in noisy {
            assert!(k.needs_attention(), "{k:?} should need attention");
        }
        for k in quiet {
            assert!(!k.needs_attention(), "{k:?} should not need attention");
        }
    }

    #[test]
    fn ratatui_style_matches_color_charter() {
        use ratatui::style::Color;
        assert_eq!(RowKind::Healthy.ratatui_style().fg, Some(Color::Green));
        assert_eq!(RowKind::Parked.ratatui_style().fg, Some(Color::Yellow));
        assert_eq!(RowKind::Hot.ratatui_style().fg, Some(Color::LightRed));
        assert_eq!(RowKind::Blocked.ratatui_style().fg, Some(Color::LightRed));
        assert_eq!(RowKind::Frozen.ratatui_style().fg, Some(Color::Cyan));
        assert_eq!(RowKind::Ghost.ratatui_style().fg, Some(Color::Red));
        assert_eq!(RowKind::Drift.ratatui_style().fg, Some(Color::Red));
        assert_eq!(RowKind::Terminal.ratatui_style().fg, Some(Color::DarkGray));
        assert_eq!(RowKind::Idle.ratatui_style().fg, Some(Color::DarkGray));
    }
}
