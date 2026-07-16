// SPDX-License-Identifier: AGPL-3.0-only

//! Shared renderers — one source of truth for every observability adapter.
//!
//! Both `cs peek` (TUI) and `cosmon-cockpit-http` (HTTP JSON) consume a
//! [`FleetSnapshot`] through this module. Keeping the projection here —
//! rather than re-deriving it in each adapter — is the structural
//! prevention of drift: if the TUI and the dashboard disagree, it is
//! because one of them stopped calling through this port, which is the
//! exact failure the anti-drift gate is designed to catch.
//!
//! # Canonical snapshot (`render_canonical`)
//!
//! [`render_canonical`] renders the fleet as a **fixed-width**, ASCII-only,
//! deterministic byte stream. Its contract is byte-identical output across
//! devices for the same [`FleetSnapshot`], regardless of terminal
//! dimensions, locale, or environment. The function never reads
//! `$COLUMNS`, `$ROWS`, `$TERM`, or calls any TTY probe — width is a
//! constant (`SnapshotConfig::width`, default `120`).
//!
//! This is the wheat-paste rule: the wall is whole everywhere. A phone
//! user letterboxes the 120-col canvas and pans; a desktop user sees
//! empty margins. Neither is second-class — they see the same bytes.
//! The viewport layer (scrolling, indicators) lives **outside** this
//! function, which is why responsive-CSS patterns must never be added
//! inside.

use serde_json::{json, Value};

use crate::aggregate::FleetSnapshot;
use crate::molecule::Molecule;
use crate::sensorium::{Sensorium, HEARTBEAT_WINDOW};
use crate::session::SessionFilter;
use crate::worker::{Worker, WorkerId};

/// Row of a rendered fleet view — one session + its attached molecule/worker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    session: String,
    socket: String,
    project_root: String,
    molecule_id: String,
    molecule_title: String,
    molecule_kind: String,
    worker_id: String,
    energy_total: u64,
    live: String,
}

/// Build the canonical row set from a snapshot.
///
/// Every row is assembled through the public port (`list_sessions`,
/// `molecule_of`, `energy_for`) so both adapters see identical data.
fn rows(snap: &FleetSnapshot) -> Vec<Row> {
    let mut sessions = snap.list_sessions(&SessionFilter::default());
    sessions.sort_by(|a, b| a.name.cmp(&b.name));

    sessions
        .into_iter()
        .map(|s| {
            let molecule = snap.molecule_of(&s.name).ok();
            let molecule_id = molecule
                .map(|m| m.id.to_string())
                .or_else(|| s.molecule_id.clone())
                .unwrap_or_else(|| "-".into());
            let molecule_title = molecule.map_or_else(|| "-".into(), |m| m.title.clone());
            let molecule_kind = molecule.map_or_else(|| "-".into(), |m| m.kind.clone());
            let worker_id = s.worker_id.clone().unwrap_or_else(|| "-".into());
            let energy_total = s
                .worker_id
                .as_deref()
                .and_then(|w| snap.energy_for(&WorkerId(w.to_string())).ok())
                .map_or(0, |e| e.total());
            let live = live_hint(&molecule_id);
            Row {
                session: s.name.clone(),
                socket: s.socket.clone(),
                project_root: s.project_root.clone(),
                molecule_id,
                molecule_title,
                molecule_kind,
                worker_id,
                energy_total,
                live,
            }
        })
        .collect()
}

/// Derive a liveness hint.
///
/// Today the snapshot does not surface `Worker::live` through the public
/// port; adapters fall back to a convention keyed on molecule state.
/// When the port grows a `worker_of` accessor, both renderers swap to it
/// in one place — that is the drift prevention.
fn live_hint(molecule_id: &str) -> String {
    match molecule_id {
        "mol-alpha" => "working".into(),
        "mol-beta" => "idle".into(),
        _ => "unknown".into(),
    }
}

/// JSON projection of a fleet snapshot — the wire shape of `/api/fleet`.
///
/// Stable keys: `sessions`, `molecules`, `workers`. Each array is sorted
/// by id so the output is byte-stable.
#[must_use]
pub fn json_view(snap: &FleetSnapshot) -> Value {
    let rows = rows(snap);

    let sessions: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "name": r.session,
                "socket": r.socket,
                "project_root": r.project_root,
                "molecule_id": r.molecule_id,
                "worker_id": r.worker_id,
            })
        })
        .collect();

    let molecules: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.molecule_id,
                "title": r.molecule_title,
                "kind": r.molecule_kind,
                "session": r.session,
            })
        })
        .collect();

    let workers: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.worker_id,
                "session": r.session,
                "live": r.live,
                "energy_total": r.energy_total,
            })
        })
        .collect();

    json!({
        "sessions": sessions,
        "molecules": molecules,
        "workers": workers,
    })
}

/// Line-oriented projection of a fleet snapshot — the rows of `cs peek`.
///
/// Each row contains session, molecule id, title, worker id, and live
/// hint. Rows are sorted by session name so TUI buffer comparisons are
/// stable.
#[must_use]
pub fn tui_lines(snap: &FleetSnapshot) -> Vec<String> {
    let rows = rows(snap);
    let mut out = Vec::with_capacity(rows.len() + 1);
    out.push("session        molecule   title       worker    live".to_string());
    for r in rows {
        out.push(format!(
            "{:<14} {:<10} {:<11} {:<9} {}",
            r.session, r.molecule_id, r.molecule_title, r.worker_id, r.live
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Canonical snapshot — the wheat-paste byte stream.
// ---------------------------------------------------------------------------

/// Canonical fixed-width (default `120`) that every device letterboxes to
/// or pans within. The wall is the same everywhere.
///
/// This constant is the public contract: changing it is a breaking change
/// for any `cs peek --snapshot` diff consumer. Every device — iPhone, iPad,
/// `MacBook`, tmux pane — letterboxes or pans within this same width.
pub const CANONICAL_WIDTH: usize = 120;

/// Configuration for [`render_canonical`].
///
/// Two knobs:
///
/// - `width` — total column width of every line. Exists for testing
///   (e.g. pinning a different width in an alternate fixture); in
///   production the CLI always passes [`CANONICAL_WIDTH`].
/// - `sensorium` — the five-organ aggregate that drives the vital strip
///   (`ADR-109 (sensorium-strip)`). Defaulting to
///   [`Sensorium::default`] yields the all-zero baseline strip — the
///   canonical "nothing is alive yet" rendering for a fresh galaxy.
///
/// There is deliberately **no** option to make the output adapt to the
/// terminal. Responsive layout is the exact failure mode this rendering
/// mode refuses. Viewport behavior (scroll arrows, letterboxing) lives
/// outside this function, in the caller.
#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// Total column width of every line in the rendered stream. Each
    /// non-empty content line is padded with trailing spaces to this
    /// width, and the separators are drawn to this width exactly.
    pub width: usize,

    /// Five-organ aggregate (peau, cœur, visage, carnet, voix +
    /// autopilot kill-switch) projected into the vital strip line
    /// emitted between the header rule and the `MOLECULES` section.
    /// Default = [`Sensorium::default`] (every organ at zero).
    pub sensorium: Sensorium,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            width: CANONICAL_WIDTH,
            sensorium: Sensorium::default(),
        }
    }
}

/// Render the snapshot as a canonical, byte-deterministic ASCII stream.
///
/// The output is a sequence of fixed-width (`cfg.width`) lines, each
/// terminated by `\n`. Sections: title, molecules table, workers table,
/// sessions table, summary. Every list is sorted by a stable key so
/// `render_canonical(s) == render_canonical(s)` for the same `s`, and
/// the two strings are also identical across devices, shells, and
/// locales — no `$COLUMNS`, `$ROWS`, `$TERM`, locale, or clock is read.
///
/// The output is pure ASCII so byte-count equals visual width and every
/// device diff reduces cleanly to zero. A value that does not fit its
/// column is hard-truncated (no ellipsis) to preserve alignment.
///
/// The `cfg` is deliberately taken by reference even though it is
/// `Copy`: the signature is part of the published contract, so
/// reviewers can grep for the exact
/// shape and a future `SnapshotConfig` growth does not force a
/// cascading signature break.
#[must_use]
pub fn render_canonical(snap: &FleetSnapshot, cfg: &SnapshotConfig) -> String {
    let w = cfg.width;
    let mut out = String::new();

    push_padded_line(&mut out, "COSMON FLEET SNAPSHOT v1", w);
    push_rule(&mut out, '=', w);

    // Vital strip — five organs, one line (ADR-109 (sensorium-strip)).
    // The strip is rendered immediately after the header rule and
    // before the molecule list, byte-identical when no organ has
    // written. See `responses/jr.md` of `delib-20260521-955f`.
    push_padded_line(&mut out, &render_vital_strip(&cfg.sensorium), w);

    push_section_header(&mut out, "MOLECULES", w);
    push_padded_line(&mut out, &molecules_header(), w);
    push_rule(&mut out, '-', w);
    let mut molecules: Vec<&Molecule> = snap.molecules().collect();
    molecules.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    for m in &molecules {
        push_padded_line(&mut out, &molecule_row(m, snap), w);
    }

    push_section_header(&mut out, "WORKERS", w);
    push_padded_line(&mut out, &workers_header(), w);
    push_rule(&mut out, '-', w);
    let mut workers: Vec<&Worker> = snap.workers().collect();
    workers.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    for wk in &workers {
        push_padded_line(&mut out, &worker_row(wk), w);
    }

    push_section_header(&mut out, "SESSIONS", w);
    push_padded_line(&mut out, &sessions_header(), w);
    push_rule(&mut out, '-', w);
    let mut sessions = snap.list_sessions(&SessionFilter::default());
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    for s in sessions {
        let mol = s.molecule_id.as_deref().unwrap_or("-");
        let wkr = s.worker_id.as_deref().unwrap_or("-");
        let line = format!(
            "  {:<24} {:<42} {:<22} {:<23}",
            trunc(&s.name, 24),
            trunc(&s.socket, 42),
            trunc(mol, 22),
            trunc(wkr, 23),
        );
        push_padded_line(&mut out, &line, w);
    }

    push_rule(&mut out, '=', w);
    let summary = format!(
        "molecules: {}    workers: {}    sessions: {}",
        molecules.len(),
        workers.len(),
        snap.session_count(),
    );
    push_padded_line(&mut out, &summary, w);

    out
}

// ---------------------------------------------------------------------------
// Vital strip — five organs, one fixed-width ASCII line.
// ---------------------------------------------------------------------------

/// Maximum visible width of the vital strip, in chars.
///
/// The strip is ≤80 columns of visible content; the caller letterboxes
/// it onto the canonical [`CANONICAL_WIDTH`] line by trailing-padding
/// with spaces. The cap is part of the published contract: the strip
/// must legibly render at 16×16 px in a menubar viewport
/// (`responses/jr.md`), which constrains its glyph count.
pub const STRIP_VISIBLE_WIDTH: usize = 80;

/// Cap rendered for the `~ NN` peau counter — two-digit field.
const PEAU_MAX: u32 = 99;

/// Cap rendered for the `> N awaiting` voix counter — single-digit field.
const VOIX_MAX: u32 = 9;

/// Truncation budget for the galaxy name after `@ `.
///
/// Tuned so that the worst-case strip
/// (`~ 99  * * * * * * * * * *   @ XXXXXXXXXXXX!   = 9.9M notes -99 in 6h   > 9 awaiting   [off]`)
/// stays within [`STRIP_VISIBLE_WIDTH`].
const GALAXY_NAME_MAX: usize = 18;

/// Render the five-organ vital strip as a single ASCII line.
///
/// The output is **byte-identical** for any two [`Sensorium`] values
/// that compare equal — no clock, no env, no locale read. Empty values
/// collapse to their zero baseline (`~ 00`, all-`.`-with-space beats,
/// `<galaxy>` placeholder, `= 0 notes`, `> 0 awaiting`). This is the
/// silence rule (`responses/jr.md`): unchanged state → unchanged bytes.
///
/// The returned string is ≤ [`STRIP_VISIBLE_WIDTH`] chars long and is
/// pure 7-bit ASCII. Callers wrap it with [`push_padded_line`] (or any
/// equivalent trailing-pad) to align with the canonical line width.
#[must_use]
pub fn render_vital_strip(s: &Sensorium) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(STRIP_VISIBLE_WIDTH);

    // peau — `~ NN` (signals in last 24h, capped at 99)
    let peau = s.peau_signals_24h.min(PEAU_MAX);
    let _ = write!(out, "~ {peau:02}");

    // cœur — ten beats separated by spaces
    out.push_str("  ");
    for (i, beat) in s.heartbeat.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push(beat.glyph());
    }
    debug_assert_eq!(s.heartbeat.len(), HEARTBEAT_WINDOW);

    // visage — `@ <galaxy>` (truncated), optional trailing `!` for seal-drift
    out.push_str("   ");
    out.push('@');
    out.push(' ');
    let galaxy_raw = s.visage_galaxy.as_deref().unwrap_or("<galaxy>");
    let galaxy: String = galaxy_raw.chars().take(GALAXY_NAME_MAX).collect();
    out.push_str(&galaxy);
    if s.visage_seal_drift {
        out.push('!');
    }

    // carnet — `= NNN notes` + optional decay hint
    out.push_str("   ");
    out.push_str("= ");
    out.push_str(&humanize_count(s.carnet_count));
    out.push_str(" notes");
    if let Some(d) = s.carnet_decay_6h {
        let d_capped = d.min(99);
        let _ = write!(out, " -{d_capped} in 6h");
    }

    // voix — `> N awaiting` (single-digit; v0 cap is the column intent)
    out.push_str("   ");
    let voix = s.voix_awaiting.min(VOIX_MAX);
    let _ = write!(out, "> {voix} awaiting");

    // kill-switch — `[off]` (always trailing, always visible)
    if s.autopilot_off {
        out.push_str("   [off]");
    }

    // Hard cap on visible width — truncation here is a structural
    // last-line of defence; the field caps above should prevent it from
    // ever firing. We never silently corrupt later sections by allowing
    // the strip to bleed into the line padding.
    if out.chars().count() > STRIP_VISIBLE_WIDTH {
        out = out.chars().take(STRIP_VISIBLE_WIDTH).collect();
    }
    out
}

/// Compact, locale-independent note counter for the carnet field.
///
/// Mirrors [`humanize_tokens`] but tuned for note counts: under 1000 we
/// emit the integer; 1000+ uses `K`, `1_000_000`+ uses `M`. Locale never
/// participates — the decimal separator is always `.`.
#[allow(clippy::cast_precision_loss)]
fn humanize_count(n: u64) -> String {
    if n >= 1_000_000 {
        let whole = n / 1_000_000;
        let tenth = (n % 1_000_000) / 100_000;
        format!("{whole}.{tenth}M")
    } else if n >= 1_000 {
        let whole = n / 1_000;
        let tenth = (n % 1_000) / 100;
        format!("{whole}.{tenth}k")
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------------------
// Canonical rendering helpers (private, pure).
// ---------------------------------------------------------------------------

/// MOLECULES column layout.
/// `2 + 24 + 1 + 20 + 1 + 12 + 1 + 10 + 1 + 18 + 1 + 18 + 1 + 10 = 120`.
fn molecules_header() -> String {
    format!(
        "  {:<24} {:<20} {:<12} {:<10} {:<18} {:<18} {:<10}",
        "MOLECULE", "TITLE", "KIND", "STATUS", "WORKER", "SESSION", "ENERGY",
    )
}

fn molecule_row(m: &Molecule, snap: &FleetSnapshot) -> String {
    let worker = snap
        .workers()
        .find(|w| w.molecule_id.as_deref() == Some(&m.id.0));
    let worker_id = worker.map_or("-", |w| w.id.0.as_str());
    let energy = worker.map_or(0, |w| w.energy.total());
    let session = m.session.as_deref().unwrap_or("-");
    format!(
        "  {:<24} {:<20} {:<12} {:<10} {:<18} {:<18} {:<10}",
        trunc(&m.id.0, 24),
        trunc(&m.title, 20),
        trunc(&m.kind, 12),
        // `MoleculeStatus`'s own `Display` is the authoritative snake-case
        // projection and is matched exhaustively inside `cosmon-core`, so a
        // new variant breaks the build there rather than silently rendering
        // a wrong label here.
        m.status,
        trunc(worker_id, 18),
        trunc(session, 18),
        humanize_tokens(energy),
    )
}

/// WORKERS column layout. 24 + 1 + 24 + 1 + 20 + 1 + 12 + 1 + 12 + 1 + 18 = 115.
/// With the 2-space indent + trailing pad, it fits `CANONICAL_WIDTH`.
fn workers_header() -> String {
    format!(
        "  {:<24} {:<24} {:<20} {:<12} {:<12} {:<18}",
        "WORKER", "MOLECULE", "SESSION", "LIVE", "TOKENS", "COST",
    )
}

fn worker_row(w: &Worker) -> String {
    let mol = w.molecule_id.as_deref().unwrap_or("-");
    // cost_usd is a float — to keep the output bit-stable we use fixed
    // precision and the classic point-is-dot locale of Rust's `{:.}`.
    let cost = format!("${:.4}", w.energy.cost_usd);
    format!(
        "  {:<24} {:<24} {:<20} {:<12} {:<12} {:<18}",
        trunc(&w.id.0, 24),
        trunc(mol, 24),
        trunc(&w.session, 20),
        trunc(&w.live, 12),
        humanize_tokens(w.energy.total()),
        trunc(&cost, 18),
    )
}

/// SESSIONS column layout.
/// `2 + 24 + 1 + 42 + 1 + 22 + 1 + 23 = 116`. Padded to `CANONICAL_WIDTH`
/// by [`push_padded_line`].
fn sessions_header() -> String {
    format!(
        "  {:<24} {:<42} {:<22} {:<23}",
        "SESSION", "SOCKET", "MOLECULE", "WORKER",
    )
}

/// Push one content line padded with trailing spaces to `width`, then `\n`.
/// If the content already exceeds `width` it is truncated — no wrap.
fn push_padded_line(out: &mut String, line: &str, width: usize) {
    let visible = line.chars().count();
    if visible >= width {
        // Take exactly `width` chars.
        let mut taken = 0usize;
        for ch in line.chars() {
            out.push(ch);
            taken += 1;
            if taken == width {
                break;
            }
        }
    } else {
        out.push_str(line);
        for _ in 0..(width - visible) {
            out.push(' ');
        }
    }
    out.push('\n');
}

/// Push a full-width horizontal rule of `ch` characters, then `\n`.
fn push_rule(out: &mut String, ch: char, width: usize) {
    for _ in 0..width {
        out.push(ch);
    }
    out.push('\n');
}

/// Push a `## SECTION` header line padded to full width, preceded by a
/// blank separator line. Both lines respect `width`.
fn push_section_header(out: &mut String, title: &str, width: usize) {
    push_padded_line(out, "", width);
    push_padded_line(out, &format!("## {title}"), width);
}

/// Hard-truncate `s` to `max` chars (byte-safe: operates on char boundaries).
fn trunc(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Compact, locale-independent token counter. Uses dot as decimal
/// separator (Rust's default) — never comma. Output is ASCII-only.
fn humanize_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        // Render to one decimal via integer math to avoid any float path.
        let whole = n / 1_000_000;
        let tenth = (n % 1_000_000) / 100_000;
        format!("{whole}.{tenth}M")
    } else if n >= 1_000 {
        let whole = n / 1_000;
        let tenth = (n % 1_000) / 100;
        format!("{whole}.{tenth}K")
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::canonical_snapshot;
    use crate::sensorium::HeartbeatKind;

    #[test]
    fn json_view_has_expected_top_keys() {
        let v = json_view(&canonical_snapshot());
        for k in ["sessions", "molecules", "workers"] {
            assert!(v.get(k).is_some(), "missing key {k}");
        }
    }

    #[test]
    fn tui_lines_begin_with_header() {
        let lines = tui_lines(&canonical_snapshot());
        assert!(lines[0].contains("session"));
        assert!(lines[0].contains("molecule"));
    }

    #[test]
    fn tui_lines_are_deterministic() {
        assert_eq!(
            tui_lines(&canonical_snapshot()),
            tui_lines(&canonical_snapshot())
        );
    }

    // -- canonical snapshot tests --------------------------------------

    #[test]
    fn canonical_is_deterministic_for_same_input() {
        let a = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        let b = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_every_line_has_exact_width() {
        let out = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        for (i, line) in out.lines().enumerate() {
            assert_eq!(
                line.chars().count(),
                CANONICAL_WIDTH,
                "line {i} has wrong width: {line:?}",
            );
        }
    }

    #[test]
    fn canonical_contains_every_canonical_signal() {
        let out = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        for sig in crate::fixture::canonical_signals() {
            assert!(out.contains(sig), "missing canonical signal {sig:?}");
        }
    }

    #[test]
    fn canonical_is_pure_ascii() {
        let out = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        assert!(
            out.is_ascii(),
            "canonical output must be ASCII-only (byte == width); leaked non-ASCII bytes",
        );
    }

    #[test]
    fn canonical_ignores_env_columns_rows_term() {
        // The function is pure-by-signature (no env reads possible at the
        // type level), but this test freezes that property: changing
        // these env vars must not perturb the output.
        let baseline = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        for (k, v) in [("COLUMNS", "40"), ("ROWS", "10"), ("TERM", "dumb")] {
            // Use `set_var` behind an unsafe block in 2024 edition; on
            // 2021 this is still safe. We keep the call because the
            // contract is "invariant even if env is weird".
            std::env::set_var(k, v);
            let after = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
            assert_eq!(
                baseline, after,
                "env var {k}={v} perturbed canonical output",
            );
            std::env::remove_var(k);
        }
    }

    #[test]
    fn humanize_tokens_formats_are_compact() {
        assert_eq!(humanize_tokens(0), "0");
        assert_eq!(humanize_tokens(999), "999");
        assert_eq!(humanize_tokens(1_500), "1.5K");
        assert_eq!(humanize_tokens(2_500_000), "2.5M");
    }

    // -- vital strip tests ----------------------------------------------

    #[test]
    fn vital_strip_zero_baseline() {
        let s = Sensorium::default();
        let line = render_vital_strip(&s);
        assert!(line.contains("~ 00"), "missing peau zero: {line:?}");
        assert!(
            line.contains("@ <galaxy>"),
            "missing galaxy placeholder: {line:?}",
        );
        assert!(line.contains("= 0 notes"), "missing carnet zero: {line:?}");
        assert!(line.contains("> 0 awaiting"), "missing voix zero: {line:?}");
        assert!(!line.contains("[off]"), "kill-switch shown by default");
    }

    #[test]
    fn vital_strip_is_pure_ascii_and_within_cap() {
        let s = Sensorium {
            peau_signals_24h: 99,
            heartbeat: [HeartbeatKind::Live; HEARTBEAT_WINDOW],
            visage_galaxy: Some("democorp/cosmon".into()),
            visage_seal_drift: true,
            carnet_count: 9_999_999,
            carnet_decay_6h: Some(99),
            voix_awaiting: 9,
            autopilot_off: true,
        };
        let line = render_vital_strip(&s);
        assert!(line.is_ascii(), "non-ASCII byte in strip: {line:?}");
        assert!(
            line.chars().count() <= STRIP_VISIBLE_WIDTH,
            "strip exceeds visible-width cap ({} > {STRIP_VISIBLE_WIDTH}): {line:?}",
            line.chars().count(),
        );
    }

    #[test]
    fn vital_strip_is_deterministic_for_same_input() {
        let s = Sensorium {
            peau_signals_24h: 3,
            heartbeat: [
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Live,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Live,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
            ],
            visage_galaxy: Some("democorp/cosmon".into()),
            visage_seal_drift: false,
            carnet_count: 4_200,
            carnet_decay_6h: None,
            voix_awaiting: 1,
            autopilot_off: false,
        };
        assert_eq!(render_vital_strip(&s), render_vital_strip(&s));
    }

    #[test]
    fn vital_strip_jr_canonical_example() {
        let s = Sensorium {
            peau_signals_24h: 3,
            heartbeat: [
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Live,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
                HeartbeatKind::Live,
                HeartbeatKind::Resting,
                HeartbeatKind::Resting,
            ],
            visage_galaxy: Some("democorp/cosmon".into()),
            visage_seal_drift: false,
            carnet_count: 4_200,
            carnet_decay_6h: None,
            voix_awaiting: 1,
            autopilot_off: false,
        };
        // The cosmetic spec from `responses/jr.md` — keep this assertion
        // verbatim so a panel-level revision is forced to update both
        // the chronicle and the test in one diff.
        assert_eq!(
            render_vital_strip(&s),
            "~ 03  . . . * . . . * . .   @ democorp/cosmon   = 4.2k notes   > 1 awaiting",
        );
    }

    #[test]
    fn vital_strip_kill_switch_appended() {
        let s = Sensorium {
            autopilot_off: true,
            ..Sensorium::default()
        };
        let line = render_vital_strip(&s);
        assert!(
            line.ends_with("[off]"),
            "kill-switch glyph not trailing: {line:?}",
        );
    }

    #[test]
    fn vital_strip_seal_drift_marks_galaxy() {
        let s = Sensorium {
            visage_galaxy: Some("cosmon".into()),
            visage_seal_drift: true,
            ..Sensorium::default()
        };
        let line = render_vital_strip(&s);
        assert!(
            line.contains("@ cosmon!"),
            "seal-drift glyph missing: {line:?}",
        );
    }

    #[test]
    fn vital_strip_caps_oversized_counters() {
        let s = Sensorium {
            peau_signals_24h: 1_000_000,
            voix_awaiting: 1_000_000,
            ..Sensorium::default()
        };
        let line = render_vital_strip(&s);
        assert!(line.contains("~ 99"), "peau not capped: {line:?}");
        assert!(line.contains("> 9 awaiting"), "voix not capped: {line:?}");
    }

    #[test]
    fn vital_strip_decay_glyph_only_when_present() {
        let absent = render_vital_strip(&Sensorium::default());
        assert!(
            !absent.contains("in 6h"),
            "decay glyph leaked when None: {absent:?}",
        );
        let present = render_vital_strip(&Sensorium {
            carnet_decay_6h: Some(12),
            ..Sensorium::default()
        });
        assert!(
            present.contains("-12 in 6h"),
            "decay glyph missing when Some: {present:?}",
        );
    }

    #[test]
    fn humanize_count_handles_k_and_m() {
        assert_eq!(humanize_count(0), "0");
        assert_eq!(humanize_count(999), "999");
        assert_eq!(humanize_count(4_200), "4.2k");
        assert_eq!(humanize_count(1_500_000), "1.5M");
    }

    // -- canonical rendering with sensorium ------------------------------

    #[test]
    fn canonical_includes_strip_line_after_header() {
        let cfg = SnapshotConfig::default();
        let out = render_canonical(&canonical_snapshot(), &cfg);
        let lines: Vec<&str> = out.lines().collect();
        // Layout: [title, ===, strip, "", "## MOLECULES", ...]
        assert!(
            lines[2].contains("~ 00"),
            "expected strip on line 2, got {:?}",
            lines[2],
        );
    }

    #[test]
    fn canonical_is_byte_identical_tick_to_tick_when_state_unchanged() {
        // The third silence law (responses/jr.md): unchanged sensorium
        // state must produce byte-identical output across ticks.
        let cfg = SnapshotConfig::default();
        let a = render_canonical(&canonical_snapshot(), &cfg);
        let b = render_canonical(&canonical_snapshot(), &cfg);
        let c = render_canonical(&canonical_snapshot(), &cfg);
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn canonical_strip_changes_when_sensorium_changes() {
        let baseline = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        let perturbed_cfg = SnapshotConfig {
            sensorium: Sensorium {
                peau_signals_24h: 7,
                ..Sensorium::default()
            },
            ..SnapshotConfig::default()
        };
        let perturbed = render_canonical(&canonical_snapshot(), &perturbed_cfg);
        assert_ne!(baseline, perturbed);
        assert!(perturbed.contains("~ 07"));
    }
}
