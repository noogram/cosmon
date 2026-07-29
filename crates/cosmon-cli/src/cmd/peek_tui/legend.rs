// SPDX-License-Identifier: AGPL-3.0-only

//! The glyph legend — page 2 of the `?` overlay.
//!
//! # Why this exists
//!
//! `cs peek` renders a dense visual vocabulary: a lifecycle pastille, a
//! whisper bubble, a temperature, a three-signal step cell, a trust bar,
//! an energy bar, role glyphs. Before this module, none of it was written
//! down anywhere the operator could reach *while looking at the table* —
//! not in the `?` overlay (keybindings only), not in `man cs`, not in the
//! handbook. The vocabulary lived in three source files. A screen whose
//! symbols can only be decoded by reading `visual.rs` has no door.
//!
//! # Two rules this module holds
//!
//! 1. **Derived, never transcribed.** Every entry calls the same renderer
//!    the table calls ([`RowKind::glyph`], [`crate::visual::temp_token`],
//!    [`Charter`],
//!    [`MoleculeHealth::glyph`], …) and iterates the same `ALL` lists. A
//!    transcribed legend rots the first time someone adds a symbol, and a
//!    stale legend is worse than none because it is believed. The test
//!    `every_rendered_glyph_appears_in_its_own_legend_section` fails when a
//!    glyph exists in a renderer and not in that renderer's own section.
//! 2. **Meanings are gestures, not enum names.** Each line says what the
//!    operator should *do*, because that is the question the glyph raised.
//!    "Waiting on something it does not control — you cannot re-prompt
//!    your way out" is the answer; `RowKind::Blocked` is not.
//!
//! # Why the overlay and not a Markdown file
//!
//! A doc file is not read while looking at a table. The overlay is the
//! source of truth for the operator's eye; `man cs` and `docs/handbook.md`
//! carry a copy for the reader who is not at the screen.

use cosmon_core::reconcile::MoleculeHealth;
use cosmon_core::visual::{Charter, Status};
use cosmon_observability::worker::WorkerRole;
use cosmon_observability::HeartbeatTier;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{glyph_for_role, molecule_health_style, WHISPER_FRESH_WINDOW};
use crate::visual::{temp_style, whisper_token, RowKind, TEMP_TOKENS};

/// Column width reserved for the glyph itself, in *character* count.
///
/// Deliberately generous: several glyphs are emoji that render two cells
/// wide, and the legend is a reading surface, not a table that must align
/// with anything else on screen.
const GLYPH_COL: usize = 4;

/// Stand-in shown in the glyph column for a column the renderer leaves
/// *empty*. An empty cell is part of the vocabulary — "no whisper", "no
/// temperature tag" — and a legend that skipped it would leave the most
/// common cell on screen undocumented. It cannot be spelled with `·`,
/// which is a real glyph with three other meanings.
const BLANK_MARKER: &str = "␣";

/// One legend row: the glyph exactly as the table renders it, the style
/// the table paints it in, and what the operator should do about it.
struct Entry {
    glyph: String,
    style: Style,
    meaning: String,
}

impl Entry {
    fn new(glyph: impl Into<String>, style: Style, meaning: impl Into<String>) -> Self {
        Self {
            glyph: glyph.into(),
            style,
            meaning: meaning.into(),
        }
    }

    fn to_line(&self) -> Line<'static> {
        let pad = GLYPH_COL.saturating_sub(self.glyph.chars().count());
        Line::from(vec![
            Span::raw("  "),
            Span::styled(self.glyph.clone(), self.style),
            Span::raw(" ".repeat(pad)),
            Span::styled(self.meaning.clone(), Style::default()),
        ])
    }
}

/// Stable identity of a legend section.
///
/// This is the key the anti-rot gate joins the glyph census to its
/// section on, and it is deliberately **not** the heading: a heading is
/// prose an editor may reword at any time, and a gate that joined on prose
/// would silently stop checking the first time someone did. It is also not
/// a position in the returned `Vec`, which reordering would break just as
/// quietly.
///
/// It is read only by the gate, which is a `#[cfg(test)]` item — so a
/// release build sees a field nobody reads and `dead_code` cannot tell a
/// join key from a forgotten one. The allow is scoped to non-test builds
/// precisely so the gate keeps proving it is used.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionId {
    /// The `♥` lifecycle pastille.
    Lifecycle,
    /// The `W` whisper column.
    Whisper,
    /// The `T` temperature column.
    Temperature,
    /// The first `● STEP` glyph — the ledger status.
    Status,
    /// The second `● STEP` glyph — molecule health.
    Health,
    /// Role glyphs printed after the molecule label.
    Roles,
    /// The `TRUST` bar.
    Trust,
    /// The `ENERGY` context bar.
    Energy,
    /// Heartbeat tiers in the expanded detail pane.
    Heartbeat,
    /// Row chrome — expand/collapse indicators and the fold.
    Chrome,
    /// The header strip. Carries prose only, no glyphs.
    Strip,
}

/// A named block of the legend: a stable id, a heading, an optional prose
/// note that explains what the *column* is for, and its glyph entries.
struct Section {
    #[cfg_attr(not(test), allow(dead_code))]
    id: SectionId,
    heading: &'static str,
    note: Vec<&'static str>,
    entries: Vec<Entry>,
}

fn heading_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn note_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// What the operator should do about each lifecycle pastille. Kept beside
/// the `match` so a new [`RowKind`] fails to compile until someone writes
/// its gesture.
fn row_kind_meaning(kind: RowKind) -> &'static str {
    match kind {
        RowKind::Healthy => "a worker is alive and producing — nothing to do",
        RowKind::Idle => {
            "at rest in its own cycle: pending, nothing blocking it. Press `t` to tackle"
        }
        RowKind::Blocked => {
            "waiting on something it does not control — an upstream molecule, or an \
             external authority refusing service. You cannot re-prompt your way out: \
             clear the blocker, rotate the credential, or wait"
        }
        RowKind::Frozen => "dormant on purpose — `cs thaw` when you want it back",
        RowKind::Ghost => {
            "the ledger says running; the tmux session is gone. Nothing is happening. \
             `cs done` to harvest, or `cs purge` to drain the roster"
        }
        RowKind::Drift => {
            "the row contradicts itself (e.g. frozen with a live worker). Only a human \
             can decide which half is true"
        }
        RowKind::Terminal => "completed or collapsed — sorted below the fold",
        // Deprecated: never produced by `classify`, so never in ALL_ACTIVE
        // and never rendered. Retired vocabulary stays out of the legend.
        RowKind::Parked | RowKind::Hot => "",
    }
}

/// What each charter status glyph asserts about the *ledger* (as opposed
/// to what the worker is actually doing, which is the health glyph).
fn status_meaning(status: Status) -> &'static str {
    match status {
        Status::Pending => "pending — created, no worker assigned yet",
        Status::Waiting => "queued — assigned, not started",
        Status::Active => "running — the ledger says a worker is on it",
        Status::Stuck => "frozen or starved — parked, or refused service by an authority",
        Status::Completed => "completed",
        Status::Collapsed => "collapsed",
    }
}

/// What the operator should do about each health verdict.
fn health_meaning(h: MoleculeHealth) -> &'static str {
    match h {
        MoleculeHealth::Healthy => "the worker is answering — the ledger is telling the truth",
        MoleculeHealth::Orphaned => "worker dead or missing while the row still reads running",
        MoleculeHealth::Stalled => {
            "alive, but its declaration went stale — check the pane with `p`"
        }
        MoleculeHealth::Blocked => {
            "pinned on a permission or trust prompt. It is waiting for a human to \
             answer a dialog — attach and answer it"
        }
        MoleculeHealth::Degraded => "the worker is in an error or paused state",
        MoleculeHealth::Inert => {
            "nothing to check yet — no worker attached. A resting state, not a fault"
        }
        MoleculeHealth::Terminal => "done — no health to report",
    }
}

/// What each heartbeat tier says about the tmux session. Red is
/// deliberately absent from `quiet`: a live pane with no recent output is
/// a quiet worker, not a broken one.
fn heartbeat_meaning(t: HeartbeatTier) -> &'static str {
    match t {
        HeartbeatTier::Active => "output within the last 30 seconds",
        HeartbeatTier::Idle => "output within the last 5 minutes",
        HeartbeatTier::Quiet | HeartbeatTier::Stalled => {
            "no output for 5 minutes or more — still alive in tmux, not broken"
        }
        HeartbeatTier::Orphaned => "no tmux session at all — the worker is gone",
    }
}

/// The role a glyph beside the molecule label advertises.
fn role_meaning(r: WorkerRole) -> &'static str {
    match r {
        WorkerRole::Cognition => "cognition worker — the process doing the thinking",
        WorkerRole::Runtime => "runtime worker — the supervisor driving a macro-molecule's DAG",
    }
}

/// Build every legend section, in reading order.
fn sections() -> Vec<Section> {
    let charter = Charter::get();
    let dim = Style::default().fg(Color::DarkGray);

    let lifecycle = Section {
        id: SectionId::Lifecycle,
        heading: "♥ — LIFECYCLE: where the molecule is in its own cycle",
        note: vec![
            "This column never shows what you decided about a molecule —",
            "only what its cycle is doing. Priority lives in T.",
        ],
        entries: RowKind::ALL_ACTIVE
            .iter()
            .map(|k| Entry::new(k.glyph(), k.ratatui_style(), row_kind_meaning(*k)))
            .collect(),
    };

    let whisper = Section {
        id: SectionId::Whisper,
        heading: "W — WHISPER: was this molecule touched by a human hand?",
        note: vec![
            "A one-bit annotation, and a decaying one. Blank means \"no whisper",
            "inside the window\" — never \"never whispered\".",
        ],
        entries: vec![
            Entry::new(
                whisper_token(true).0,
                whisper_token(true).1,
                format!(
                    "a whisper landed within the last {} minutes",
                    WHISPER_FRESH_WINDOW.num_minutes()
                ),
            ),
            Entry::new(
                BLANK_MARKER,
                dim,
                format!(
                    "empty cell — no whisper in the last {} minutes",
                    WHISPER_FRESH_WINDOW.num_minutes()
                ),
            ),
        ],
    };

    let temperature = Section {
        id: SectionId::Temperature,
        heading: "T — TEMPERATURE: what YOU decided (the `temp:*` tag)",
        note: vec![
            "Orthogonal to ♥ on purpose: a hot molecule that is idle shows 🔥 here",
            "and 💤 there. If one signal ever appears in both columns, that is the",
            "bug — the chronicle calls it \"La flamme qui doublait\".",
            "Note 🧊 means frozen in BOTH columns, but not the same frozen: in ♥ it",
            "is the molecule's status, in T it is your temperature tag.",
        ],
        entries: TEMP_TOKENS
            .iter()
            .map(|(tag, glyph)| {
                Entry::new(
                    *glyph,
                    temp_style(tag),
                    match *tag {
                        "temp:hot" => format!("{tag} — do this next"),
                        "temp:warm" => format!("{tag} — real, not now"),
                        "temp:cold" => format!("{tag} — parked, no date"),
                        _ => format!("{tag} — shelved"),
                    },
                )
            })
            .chain(std::iter::once(Entry::new(
                BLANK_MARKER,
                dim,
                "empty cell — no temp:* tag. Nobody has triaged it",
            )))
            .collect(),
    };

    let status = Section {
        id: SectionId::Status,
        heading: "● STEP — three signals packed in one column",
        note: vec![
            "Reads left to right: <what the ledger says> <what the worker is doing>",
            "<step counter>. When the first two disagree, believe the second: the",
            "ledger is a record, the health is a probe.",
            "",
            "First glyph — the ledger:",
        ],
        entries: Status::ALL
            .iter()
            .map(|s| {
                Entry::new(
                    charter.status(*s).glyph.clone(),
                    Style::default(),
                    status_meaning(*s),
                )
            })
            .collect(),
    };

    let health = Section {
        id: SectionId::Health,
        heading: "● STEP — second glyph: molecule health (the probe)",
        note: vec![],
        entries: MoleculeHealth::ALL
            .iter()
            .map(|h| Entry::new(h.glyph(), molecule_health_style(*h), health_meaning(*h)))
            .collect(),
    };

    let roles = Section {
        id: SectionId::Roles,
        heading: "MOLECULE — role glyphs after the label",
        note: vec![],
        entries: WorkerRole::ALL
            .iter()
            .map(|r| Entry::new(glyph_for_role(*r), Style::default(), role_meaning(*r)))
            .collect(),
    };

    let trust = Section {
        id: SectionId::Trust,
        heading: "TRUST — how much of this molecule's lineage is verified",
        note: vec![
            "The bar is the number; the digits are there to confirm it. A missing",
            "score is not a low score — nobody has run the check yet.",
        ],
        entries: vec![
            Entry::new(
                "—",
                Style::default().fg(Color::DarkGray),
                "not verified yet — no lineage check has run (NOT the same as 0%)",
            ),
            Entry::new(
                "██",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                "below 50% — the evidence chain is broken",
            ),
            Entry::new(
                "▓░",
                Style::default().fg(Color::Yellow),
                "50–85% — partially verified",
            ),
            Entry::new(
                "██",
                Style::default().fg(Color::Green),
                "above 85% — strong lineage",
            ),
        ],
    };

    let energy = Section {
        id: SectionId::Energy,
        heading: "ENERGY — <context bar> <tokens> <cost>",
        note: vec![
            "The bar is how full the worker's context window is, not how much it",
            "has spent. A lone `·` is the one that bites: it means no context",
            "window was REPORTED — unknown, not zero, not empty.",
        ],
        entries: vec![
            Entry::new("▂", Style::default(), "context window under 25% full"),
            Entry::new("▄", Style::default(), "25–50%"),
            Entry::new("▆", Style::default(), "50–75%"),
            Entry::new(
                "█",
                Style::default(),
                "over 75% — the worker is running out of room",
            ),
            Entry::new(
                "·",
                dim,
                "no context window reported — UNKNOWN, not 0%. The adapter did not say",
            ),
        ],
    };

    let heartbeat = Section {
        id: SectionId::Heartbeat,
        heading: "Heartbeat — shown in the expanded detail pane (→ to expand)",
        note: vec![],
        entries: HeartbeatTier::ALL
            .iter()
            .map(|t| Entry::new(t.glyph(), Style::default(), heartbeat_meaning(*t)))
            .collect(),
    };

    let chrome = Section {
        id: SectionId::Chrome,
        heading: "Row chrome",
        note: vec![],
        entries: vec![
            Entry::new("▸", dim, "collapsed row — press → to expand it"),
            Entry::new("▾", dim, "expanded row — press ← to collapse it"),
            Entry::new(
                "───",
                dim,
                "the fold — everything under it is terminal (completed / collapsed)",
            ),
        ],
    };

    let strip = Section {
        id: SectionId::Strip,
        heading: "Header strip — workers: N registered · N attached · N phantom",
        note: vec![
            "A phantom is a roster entry with no live tmux session behind it. It",
            "costs nothing and it lies about how big your fleet is. When the count",
            "is above zero the strip names the remedy itself: `cs purge`. The strip",
            "counts, never lists — one number beats twenty-seven rows — and no flag",
            "can reveal a phantom, because the flags filter molecules and a phantom",
            "is not one.",
        ],
        entries: vec![],
    };

    vec![
        lifecycle,
        whisper,
        temperature,
        status,
        health,
        roles,
        trust,
        energy,
        heartbeat,
        chrome,
        strip,
    ]
}

/// Build the glyph-legend page of the `?` overlay as styled lines.
///
/// Every glyph here is produced by calling the renderer the table itself
/// calls, so the legend cannot drift from the screen it explains.
pub(crate) fn help_legend_lines() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, section) in sections().iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(section.heading, heading_style())));
        for n in &section.note {
            lines.push(Line::from(Span::styled(format!("  {n}"), note_style())));
        }
        // Two enum variants can legitimately share one glyph and one
        // gesture — `Quiet` and `Stalled` are both "still alive, just not
        // talking". Printing that row twice teaches the operator that two
        // different things look alike, which is the opposite of true.
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for e in &section.entries {
            let key = (e.glyph.as_str(), e.meaning.as_str());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            lines.push(e.to_line());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual::temp_token;

    /// Flatten the legend into plain text for containment assertions.
    fn legend_text() -> String {
        help_legend_lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every glyph the running renderers can put on screen, paired with
    /// the renderer that produces it.
    ///
    /// This is the census the legend is checked against. It is built by
    /// *calling* the renderers, not by transcribing their output, so a
    /// changed glyph moves here automatically; only a changed **set** of
    /// variants requires a human, and the `ALL` lists it iterates are
    /// each pinned by an exhaustive-match test in their own crate.
    fn rendered_glyphs() -> Vec<(SectionId, &'static str, String)> {
        let charter = Charter::get();
        let mut out: Vec<(SectionId, &'static str, String)> = Vec::new();

        for k in RowKind::ALL_ACTIVE {
            out.push((
                SectionId::Lifecycle,
                "RowKind (♥ column)",
                k.glyph().to_owned(),
            ));
        }
        out.push((
            SectionId::Whisper,
            "whisper_token (W column)",
            whisper_token(true).0.to_owned(),
        ));
        for (tag, _) in TEMP_TOKENS {
            let tags = [(*tag).to_owned()];
            out.push((
                SectionId::Temperature,
                "temp_token (T column)",
                temp_token(&tags).0.to_owned(),
            ));
        }
        for s in Status::ALL {
            out.push((
                SectionId::Status,
                "charter status (● STEP)",
                charter.status(*s).glyph.clone(),
            ));
        }
        for h in MoleculeHealth::ALL {
            out.push((
                SectionId::Health,
                "MoleculeHealth (● STEP)",
                h.glyph().to_owned(),
            ));
        }
        for r in WorkerRole::ALL {
            out.push((
                SectionId::Roles,
                "glyph_for_role (MOLECULE)",
                glyph_for_role(*r).to_owned(),
            ));
        }
        for t in HeartbeatTier::ALL {
            out.push((
                SectionId::Heartbeat,
                "HeartbeatTier (detail pane)",
                t.glyph().to_owned(),
            ));
        }
        // trust_badge — the bar prefixes across all four bands.
        for score in [None, Some(10_u8), Some(70), Some(99)] {
            let badge = super::super::trust_badge(score).0;
            let bar: String = badge
                .trim()
                .chars()
                .take_while(|c| !c.is_ascii_digit())
                .collect();
            out.push((
                SectionId::Trust,
                "trust_badge (TRUST)",
                bar.trim().to_owned(),
            ));
        }
        // format_energy — one sample per bar bucket, plus the unreported case.
        for (total, cw) in [
            (10_u64, Some(100_u64)),
            (30, Some(100)),
            (60, Some(100)),
            (90, Some(100)),
            (10, None),
        ] {
            let cell = super::super::format_energy(total, 0, 0.0, cw);
            let bar: String = cell.trim_start().chars().take(1).collect();
            out.push((SectionId::Energy, "format_energy (ENERGY)", bar));
        }
        out.push((SectionId::Chrome, "expand indicator", "▸".to_owned()));
        out.push((SectionId::Chrome, "expand indicator", "▾".to_owned()));
        out
    }

    /// **The anti-rot gate.** A glyph that the renderers can put on screen
    /// and the legend does not carry *under the heading a reader would
    /// consult* is a symbol with no door — exactly the defect this module
    /// was written to close. Adding a symbol to a renderer without adding
    /// it to that renderer's own section fails this test.
    ///
    /// The assertion is deliberately **per-section**, not against the
    /// flattened page. A containment check over the whole text passes when
    /// a glyph produced by the `W` column is documented only under
    /// `ENERGY` — it proves the character appears *somewhere*, which is
    /// not the property the legend exists to hold. The census already
    /// carries the section each renderer feeds; joining on it is what
    /// makes the gate a gate.
    #[test]
    fn every_rendered_glyph_appears_in_its_own_legend_section() {
        let sections = sections();
        for (id, source, glyph) in rendered_glyphs() {
            let Some(section) = sections.iter().find(|s| s.id == id) else {
                panic!("legend has no section {id:?} — the census names one that sections() does not build");
            };
            assert!(
                section.entries.iter().any(|e| e.glyph == glyph),
                "glyph {glyph:?} is rendered by {source} but has no entry under \
                 the legend section {:?} ({:?}) — the heading a reader looking at \
                 that column would open. Add it there in \
                 crates/cosmon-cli/src/cmd/peek_tui/legend.rs; documenting it \
                 under some other heading does not give it a door. A stale \
                 legend is worse than none, because it is believed.",
                id,
                section.heading,
            );
        }
    }

    /// The gate above is only worth its runtime if a *misfiled* glyph reds
    /// it. This proves it does, without touching the production census: it
    /// asks the same question the gate asks — "is this glyph an entry of
    /// THAT section?" — about a glyph filed under the wrong one.
    ///
    /// `🔥` is real vocabulary: `temp_token` renders it in the `T` column,
    /// so a whole-page containment check finds it and passes. Asked about
    /// the `ENERGY` section, which never carries it, the per-section
    /// question answers no. That difference is exactly what this test
    /// pins: had the gate stayed a `text.contains`, this assertion would
    /// be unwritable.
    #[test]
    fn a_glyph_filed_under_the_wrong_section_is_not_accepted() {
        let sections = sections();
        let text = legend_text();
        let misfiled = "🔥";

        assert!(
            text.contains(misfiled),
            "precondition: {misfiled:?} must be somewhere in the legend, or this \
             test is not demonstrating the difference it claims to",
        );
        let Some(energy) = sections.iter().find(|s| s.id == SectionId::Energy) else {
            panic!("legend has no ENERGY section");
        };
        assert!(
            !energy.entries.iter().any(|e| e.glyph == misfiled),
            "the per-section gate must reject a glyph documented under a \
             section that does not render it — if this ever holds, the ENERGY \
             section grew a temperature glyph and the gate has stopped \
             discriminating",
        );
    }

    /// Retired vocabulary must stay out. `Parked` and `Hot` are never
    /// produced by `classify`, so teaching them would teach a dialect the
    /// screen does not speak.
    #[test]
    fn legend_omits_deprecated_row_kinds() {
        let text = legend_text().to_lowercase();
        assert!(
            !text.contains("parked —"),
            "deprecated RowKind::Parked leaked"
        );
        assert!(
            !text.contains("temp:hot in the ♥"),
            "deprecated RowKind::Hot leaked",
        );
    }

    /// The whisper window is a decaying signal; a glyph that decays
    /// without naming its clock is a clock with no face.
    #[test]
    fn legend_names_the_whisper_window() {
        let text = legend_text();
        let minutes = WHISPER_FRESH_WINDOW.num_minutes().to_string();
        assert!(
            text.contains(&format!("{minutes} minutes")),
            "the W column's freshness window must be named in the legend",
        );
    }

    /// The lone `·` in ENERGY means "not reported", which is not "zero".
    /// The legend must separate them or the dot is unreadable.
    #[test]
    fn legend_separates_unreported_from_zero() {
        let text = legend_text();
        assert!(
            text.contains("UNKNOWN, not 0%"),
            "the energy column's lone `·` must be distinguished from 0%",
        );
    }

    /// The T/♥ orthogonality cost a chronicle entry; the legend states it
    /// rather than leaving the operator to rediscover it.
    #[test]
    fn legend_states_the_orthogonality() {
        let text = legend_text();
        assert!(text.contains("Orthogonal to ♥"));
        assert!(text.contains("La flamme qui doublait"));
    }

    /// A column carrying three signals must say it carries three.
    #[test]
    fn legend_announces_the_three_signals_in_step() {
        let text = legend_text();
        assert!(text.contains("three signals packed in one column"));
    }

    /// An empty W or T cell is the most common cell on screen. Leaving it
    /// out of the legend would document every rare state and skip the
    /// usual one.
    #[test]
    fn legend_documents_the_empty_cell() {
        let text = legend_text();
        assert_eq!(
            text.matches("empty cell").count(),
            2,
            "both the W and T columns must explain their blank",
        );
    }

    /// The blank stand-in must not collide with a glyph the screen can
    /// actually render, or the legend would teach a symbol that means
    /// something else in the table.
    #[test]
    fn blank_marker_is_not_a_rendered_glyph() {
        for (_, source, glyph) in rendered_glyphs() {
            assert_ne!(
                glyph, BLANK_MARKER,
                "{source} renders the blank stand-in {BLANK_MARKER:?} — pick another",
            );
        }
    }

    /// The header's phantom count is only useful with its remedy attached.
    #[test]
    fn legend_names_the_phantom_remedy() {
        let text = legend_text();
        assert!(text.contains("phantom"));
        assert!(text.contains("cs purge"));
    }

    /// Two variants that share a glyph *and* a gesture must print once —
    /// a duplicated row claims two things look alike when they are the
    /// same thing.
    #[test]
    fn legend_prints_each_glyph_gesture_pair_once() {
        let lines: Vec<String> = help_legend_lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .filter(|s| !s.trim().is_empty())
            .collect();
        let mut sorted = lines.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            lines.len(),
            "the legend repeats a line — dedupe it in `help_legend_lines`",
        );
    }
}
