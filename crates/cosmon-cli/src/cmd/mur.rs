// SPDX-License-Identifier: AGPL-3.0-only

//! `cs mur` — the Mur du Matin fresque.
//!
//! A one-operator morning dashboard that projects the cluster of galaxies
//! across four family bands with a seven-channel visual encoding.
//!
//! # The picture
//!
//! The fresque is *not* a 2×2 grid. Four horizontal bands are stacked
//! top-to-bottom, with heights proportional to cluster reality
//! (1 infra, 7 projects, 2 hubs, 1 editorial — not equal). Social-hub and
//! editorial share the last line with an **empty vertical margin** between
//! them — the empty space is information (drift guard-rail). Positions are
//! fixed per galaxy so the operator memorises addresses.
//!
//! # The seven channels
//!
//! 1. **Size** — `log(activity)` floored so a silent galaxy is never
//!    invisible.
//! 2. **Color** — health, with a palette **per family**:
//!    - infra: blue (stable) → red (drift)
//!    - project: green → yellow → red
//!    - social-hub: saturated red (alive) → grey (silent = dead)
//!    - editorial: ink (published) → empty cream (silent)
//! 3. **Position** — fixed per galaxy.
//! 4. **Halo thickness** — North Star success (log-scaled). Rendered as the
//!    bracket style around the tile: `═[name]═` (thick) / `─[name]─`
//!    (thin) / `·[name]·` (faint).
//! 5. **Motion** — reserved for drift-alert (last resort). The terminal
//!    renderer cannot animate, so we emit `*` as a stationary indicator.
//! 6. **Delta overlay** — jobs's **one** red dot. Exactly the single galaxy
//!    whose North Star moved > weekly variance yesterday. Zero otherwise.
//! 7. **Measurement-ceiling tint** — carnot's honesty rule: quantities that
//!    cannot be directly measured (editorial reach, social-hub authenticity,
//!    project principle-value) are rendered with a **dashed halo**
//!    (`╌[name]╌`) instead of a sharp line.
//!
//! # Data sources
//!
//! All metrics pull from existing stores — no new pipelines:
//!
//! - **galaxy taxonomy**: neurion `repos.galaxy_kind`.
//! - **activity**: `git log --since=7.days` on each galaxy's `local_path`.
//! - **success (stub)**: the same git-log count, log-scaled — the formula
//!   will be upgraded when the metrics catalogue lands (§6).
//! - **delta (stub)**: today's commits vs the 7-day average.

// Small counts (commit rates in the ones-to-hundreds) — precision loss
// on u64→f64 is immaterial, and casting f64→usize (tile widths,
// padding) is bounded by explicit clamps below. Disabling the
// clippy pedant for those pays for readability without masking the
// cast sites.
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

use colored::{ColoredString, Colorize};
use neurion_core::GalaxyKind;
use rusqlite::Connection;
use serde::Serialize;

use super::Context;

/// Arguments for `cs mur`.
#[derive(clap::Args)]
pub struct Args {
    /// Render the fresque as a single-shot snapshot (no TUI yet).
    ///
    /// This flag is accepted for forward-compatibility: a future TUI tab
    /// may invert the default to an interactive view. Today it is the
    /// default and the only mode.
    #[arg(long)]
    pub snapshot: bool,
}

/// One row of the Mur — a galaxy with the computed visual encoding.
#[derive(Serialize)]
#[allow(clippy::struct_field_names)]
struct Tile {
    /// Canonical galaxy name.
    pub name: String,
    /// Family — `infra | project | social-hub | editorial`.
    pub kind: String,
    /// Filesystem path from neurion `repos.local_path`.
    pub local_path: String,
    /// Commits in the last 7 days on the default branch (best-effort).
    pub activity_7d: u64,
    /// Commits in the last 24h — used to pick the delta leader.
    pub activity_1d: u64,
    /// Tile width = `log1p(activity_7d)` clamped to `[MIN_TILE, MAX_TILE]`.
    pub tile_width: usize,
    /// North Star value (stub: `log1p(activity_7d)`). Re-assigned once the
    /// metrics catalogue (synthesis §6) is plumbed in.
    pub success: f64,
    /// Health score, `[0, 1]`. Interpretation is family-dependent — see
    /// [`health_score`].
    pub health: f64,
    /// Standardised delta (`activity_1d` vs 7-day average). The galaxy with
    /// the highest `|delta|` above the weekly-variance threshold earns the
    /// red dot; the rest render without it.
    pub delta: f64,
    /// Per synthesis §6: the family's North Star metric is
    /// measurement-ceiling-bounded (editorial reach, social-hub
    /// authenticity, project principle-value). Rendered with a dashed
    /// halo instead of a solid line.
    pub measurement_ceiling: bool,
    /// `true` for the single galaxy that wins the delta ranking.
    pub is_delta_leader: bool,
}

/// `--json` envelope.
#[derive(Serialize)]
struct MurOutput {
    /// All tiles, flat, ordered by family then by fixed-position ordinal.
    pub tiles: Vec<Tile>,
    /// Per-family counts (`infra = 1`, …). Same shape as `cs galaxies list`.
    pub by_kind: BTreeMap<String, usize>,
    /// The name of the delta-leader galaxy, or `None` if no galaxy exceeds
    /// the weekly-variance threshold.
    pub delta_leader: Option<String>,
}

/// Dispatch `cs mur`.
pub fn run(ctx: &Context, _args: &Args) -> anyhow::Result<()> {
    let tiles = collect_tiles()?;

    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for t in &tiles {
        *by_kind.entry(t.kind.clone()).or_insert(0) += 1;
    }
    let delta_leader = tiles
        .iter()
        .find(|t| t.is_delta_leader)
        .map(|t| t.name.clone());

    if ctx.json {
        let out = MurOutput {
            tiles,
            by_kind,
            delta_leader,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    render_fresque(&tiles, delta_leader.as_deref());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Data collection
// ─────────────────────────────────────────────────────────────────────

const MIN_TILE: usize = 6;
const MAX_TILE: usize = 22;

/// Weekly-variance threshold (in standardised units) above which a
/// galaxy becomes the delta-of-day candidate. Kept deliberately loose —
/// we want ONE red dot per day at most, not a christmas tree.
const DELTA_THRESHOLD: f64 = 1.5;

/// Read galaxies + their local paths from the neurion DB and compute one
/// [`Tile`] per classified galaxy. Nascent galaxies are skipped — the
/// fresque is about the four families; classification is the prerequisite.
fn collect_tiles() -> anyhow::Result<Vec<Tile>> {
    let raw = load_classified_galaxies()?;

    // First pass: pull raw activity numbers so we can compute a variance
    // threshold for the delta overlay.
    let mut rows: Vec<RawRow> = raw
        .into_iter()
        .map(|r| {
            let a7 = git_commit_count(&r.local_path, "7 days ago").unwrap_or(0);
            let a1 = git_commit_count(&r.local_path, "1 day ago").unwrap_or(0);
            RawRow {
                name: r.name,
                kind: r.kind,
                local_path: r.local_path,
                activity_7d: a7,
                activity_1d: a1,
            }
        })
        .collect();

    // Sort into canonical fixed-position order so the operator can
    // memorise addresses. Sort key: family ordinal → name.
    rows.sort_by(|a, b| {
        family_ordinal(&a.kind)
            .cmp(&family_ordinal(&b.kind))
            .then_with(|| a.name.cmp(&b.name))
    });

    // Compute delta = (activity_1d − mean_per_day) / stddev, where
    // mean_per_day = activity_7d / 7. Pure stub, replaced once §6
    // metrics land, but honest enough to point at a delta-of-day today.
    let deltas: Vec<f64> = rows.iter().map(standardised_delta).collect();
    let leader_name: Option<String> = rows
        .iter()
        .zip(deltas.iter())
        .max_by(|a, b| {
            a.1.abs()
                .partial_cmp(&b.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .and_then(|(row, d)| {
            if d.abs() > DELTA_THRESHOLD {
                Some(row.name.clone())
            } else {
                None
            }
        });

    let tiles = rows
        .into_iter()
        .zip(deltas)
        .map(|(r, delta)| {
            let success = (r.activity_7d as f64 + 1.0).ln();
            let is_delta_leader = leader_name.as_deref() == Some(r.name.as_str());
            let tile_width = tile_width_for(r.activity_7d);
            let measurement_ceiling = family_has_measurement_ceiling(&r.kind);
            let health = health_score(&r.kind, r.activity_7d, r.activity_1d);
            Tile {
                name: r.name,
                kind: r.kind,
                local_path: r.local_path,
                activity_7d: r.activity_7d,
                activity_1d: r.activity_1d,
                tile_width,
                success,
                health,
                delta,
                measurement_ceiling,
                is_delta_leader,
            }
        })
        .collect();

    Ok(tiles)
}

/// One row from neurion + the family tag.
struct RawRow {
    name: String,
    kind: String,
    local_path: String,
    activity_7d: u64,
    activity_1d: u64,
}

/// Poisson-ish standardised delta — `(today − mean) / sqrt(mean)`. Safe
/// on zero activity via `mean.max(1.0)`.
fn standardised_delta(row: &RawRow) -> f64 {
    let mean = (row.activity_7d as f64) / 7.0;
    let stddev = mean.max(1.0).sqrt();
    ((row.activity_1d as f64) - mean) / stddev
}

/// Raw galaxy row exactly as neurion stores it — only the fields the
/// Mur needs.
struct Galaxy {
    name: String,
    kind: String,
    local_path: String,
}

/// Read every classified galaxy (`galaxy_kind IS NOT NULL`) from the
/// neurion `repos` table. Shape mirrors [`super::galaxies::load_galaxies`]
/// but narrows to the classified set — the Mur only paints the four
/// families.
fn load_classified_galaxies() -> anyhow::Result<Vec<Galaxy>> {
    let db = neurion_db_path()?;
    if !db.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open(&db)?;

    // Same pre-migration tolerance as `cs galaxies list` — if the column
    // hasn't been added yet, there are simply no classified galaxies to
    // paint and the Mur renders an empty fresque.
    let probe = conn
        .prepare("SELECT galaxy_kind FROM repos LIMIT 1")
        .is_ok();
    if !probe {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT name, galaxy_kind, local_path
         FROM repos
         WHERE galaxy_kind IS NOT NULL
         ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let kind: Option<String> = row.get(1)?;
        let path: String = row.get(2)?;
        Ok((name, kind, path))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (name, kind, path) = row?;
        let Some(k) = kind else {
            continue;
        };
        // Guard: only accept the closed enum's four tokens. Free-text
        // would break the band layout.
        if GalaxyKind::from_str(&k).is_none() {
            continue;
        }
        out.push(Galaxy {
            name,
            kind: k,
            local_path: path,
        });
    }
    Ok(out)
}

/// Canonical neurion `SQLite` DB path. Same logic as `cs galaxies list`.
fn neurion_db_path() -> anyhow::Result<PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine data directory"))?
        .join("neurion");
    Ok(dir.join("neurion.db"))
}

/// Best-effort commit count since the given `git log --since` argument.
///
/// Silent on error: an unreachable repo, a non-git directory, or a
/// detached submodule all yield `0`. The Mur must render even when the
/// filesystem is partially unavailable.
fn git_commit_count(path: &str, since: &str) -> Option<u64> {
    let p = Path::new(path);
    if !p.exists() {
        return None;
    }
    let output = ShellCommand::new("git")
        .current_dir(p)
        .args(["log", "--oneline", &format!("--since={since}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let n = stdout.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    Some(n)
}

// ─────────────────────────────────────────────────────────────────────
// Visual encoding
// ─────────────────────────────────────────────────────────────────────

fn tile_width_for(activity: u64) -> usize {
    let raw = ((activity as f64) + 1.0).ln() * 4.0;
    let width = raw.round() as usize + MIN_TILE;
    width.clamp(MIN_TILE, MAX_TILE)
}

/// Family ordinal for fixed-position sorting — smaller = painted first
/// (top band).
fn family_ordinal(kind: &str) -> u8 {
    match kind {
        "infra" => 0,
        "project" => 1,
        "social-hub" => 2,
        "editorial" => 3,
        _ => 4,
    }
}

/// Per-family health score in `[0, 1]`. Higher = healthier for the
/// family's own physics. Pure stub today — see synthesis §6 for the
/// metrics catalogue that will replace this.
fn health_score(kind: &str, a7: u64, a1: u64) -> f64 {
    // A 7-day activity of ~5 commits is the reference for "alive".
    let base = ((a7 as f64) / 5.0).min(1.0);
    let today = if a1 > 0 { 1.0 } else { 0.6 };
    match kind {
        // Infra: *too much* change is unhealthy. We invert the base so a
        // stable infra sits at 0.8–1.0. Not perfect but the palette is
        // symmetric (blue→red) so the operator will read drift as the
        // shift toward red.
        "infra" => (1.0 - base).clamp(0.2, 0.9),
        // Project: linear — green when active, red when stalled.
        "project" => 0.5 * base + 0.5 * today,
        // Social-hub: silence is death. Any activity in 24h keeps it
        // saturated; silence over 7 days pushes it to grey.
        "social-hub" => {
            if a1 > 0 {
                1.0
            } else {
                base * 0.5
            }
        }
        // Editorial: publications matter more than commits, but we
        // don't have the publication tracker yet. Stub: same curve as
        // project but dimmer so the tile reads as "ink on cream".
        "editorial" => base.max(0.1),
        _ => base,
    }
}

/// True for families whose North Star metric is measurement-ceiling-bounded.
/// Synthesis §6:
/// - editorial reach (external audience ≠ publication count)
/// - social-hub authenticity (all log proxies are noisy)
/// - project principle-value (revealed over months, not days)
///
/// Infra is *not* measurement-ceiling-bounded — adoption is directly
/// observable (sister commits / issues / semver breaks).
fn family_has_measurement_ceiling(kind: &str) -> bool {
    matches!(kind, "editorial" | "social-hub" | "project")
}

/// ANSI-coloured tile label — applies the per-family palette driven by
/// the `health` score. Colour is the signal; the text is identical to
/// the bare name.
fn colored_label(name: &str, kind: &str, health: f64) -> ColoredString {
    // health ∈ [0, 1]: 1 = healthy for the family, 0 = family-specific
    // pathology.
    match kind {
        // infra: blue (stable) → red (drift). Healthy = blue.
        "infra" => {
            if health >= 0.6 {
                name.bright_blue().bold()
            } else if health >= 0.3 {
                name.yellow().bold()
            } else {
                name.bright_red().bold()
            }
        }
        // project: green → yellow → red.
        "project" => {
            if health >= 0.7 {
                name.bright_green().bold()
            } else if health >= 0.3 {
                name.yellow().bold()
            } else {
                name.red().bold()
            }
        }
        // social-hub: saturated red (alive) → grey (silent).
        "social-hub" => {
            if health >= 0.6 {
                name.bright_red().bold()
            } else {
                name.dimmed()
            }
        }
        // editorial: ink on cream (published) → empty cream (silent).
        // In a terminal: normal text for ink, dimmed for absence.
        "editorial" => {
            if health >= 0.5 {
                name.bold()
            } else {
                name.dimmed()
            }
        }
        _ => name.normal(),
    }
}

/// Pick the halo brackets for a tile. Three levels of solid halo + one
/// dashed level for measurement-ceiling quantities.
///
/// Returns `(left, right)` bracket strings.
fn halo_brackets(success: f64, ceiling: bool) -> (&'static str, &'static str) {
    if ceiling {
        // Dashed halo — carnot's honesty rule. Same three strengths but
        // rendered as broken lines so the eye reads "we can't measure
        // this directly".
        if success >= 2.5 {
            ("╌╌[", "]╌╌")
        } else if success >= 1.2 {
            ("╌[", "]╌")
        } else {
            ("·[", "]·")
        }
    } else if success >= 2.5 {
        ("══[", "]══")
    } else if success >= 1.2 {
        ("──[", "]──")
    } else {
        ("··[", "]··")
    }
}

// ─────────────────────────────────────────────────────────────────────
// Rendering
// ─────────────────────────────────────────────────────────────────────

const BAND_WIDTH: usize = 82;

fn render_fresque(tiles: &[Tile], delta_leader: Option<&str>) {
    println!("{}", "🌌 Mur du Matin".bold());
    println!(
        "{}",
        format!("   {} — fresque en 4 bandes", today_stamp()).dimmed()
    );
    println!();

    // Band 1 — INFRA (thin, cool)
    render_band_header("INFRA", "bits flow inward", '═');
    let infra = tiles_for_kind(tiles, "infra");
    render_band_row(&infra, delta_leader, true);
    println!();

    // Band 2 — PROJECT (wide, central)
    render_band_header("PROJECT", "bits flow through", '─');
    for row in chunk_tiles(tiles_for_kind(tiles, "project"), 3) {
        render_band_row(&row, delta_leader, false);
    }
    println!();

    // Band 3 — SOCIAL-HUB || EDITORIAL (side by side with an empty margin)
    render_split_band_header("SOCIAL-HUB", "EDITORIAL", '─');
    let hub = tiles_for_kind(tiles, "social-hub");
    let editorial = tiles_for_kind(tiles, "editorial");
    render_split_band(&hub, &editorial, delta_leader);
    println!();

    render_legend(delta_leader);
}

fn today_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn tiles_for_kind<'a>(tiles: &'a [Tile], kind: &str) -> Vec<&'a Tile> {
    tiles.iter().filter(|t| t.kind == kind).collect()
}

fn chunk_tiles(mut tiles: Vec<&Tile>, per_row: usize) -> Vec<Vec<&Tile>> {
    let mut out = Vec::new();
    while !tiles.is_empty() {
        let take = per_row.min(tiles.len());
        let rest = tiles.split_off(take);
        out.push(tiles);
        tiles = rest;
    }
    out
}

fn render_band_header(label: &str, tagline: &str, fill: char) {
    let header_left = format!("{fill}{fill}{fill} {label} {fill}{fill}{fill}");
    let header_left_width = header_left.chars().count();
    let pad = BAND_WIDTH.saturating_sub(header_left_width + tagline.len() + 3);
    let fill_run: String = std::iter::repeat_n(fill, pad).collect();
    println!(
        "{}{}  {}",
        header_left.bold(),
        fill_run.dimmed(),
        tagline.dimmed()
    );
}

fn render_split_band_header(left: &str, right: &str, fill: char) {
    // social-hub header on the left, editorial on the right, with an
    // empty vertical margin (` ║ `) between them — synthesis §7: the
    // margin is the drift guard-rail.
    let left_text = format!("{fill}{fill}{fill} {left} {fill}{fill}{fill}");
    let right_text = format!("{fill}{fill}{fill} {right} {fill}{fill}{fill}");
    let half = BAND_WIDTH / 2;
    let left_pad = half.saturating_sub(left_text.chars().count());
    let right_pad = half.saturating_sub(right_text.chars().count());
    let left_fill: String = std::iter::repeat_n(fill, left_pad).collect();
    let right_fill: String = std::iter::repeat_n(fill, right_pad).collect();
    println!(
        "{}{} ║ {}{}",
        left_text.bold(),
        left_fill.dimmed(),
        right_text.bold(),
        right_fill.dimmed(),
    );
}

fn render_band_row(row: &[&Tile], delta_leader: Option<&str>, _center: bool) {
    if row.is_empty() {
        // An empty band is information: "no galaxy of this family yet".
        // We do not silently skip it — print a faint placeholder so the
        // fresque's band structure is preserved.
        let msg = "   (vacant)".dimmed();
        println!("  {msg}");
        return;
    }
    let line = row
        .iter()
        .map(|t| render_tile(t, delta_leader))
        .collect::<Vec<_>>()
        .join("  ");
    // The infra band holds a single galaxy; the project band has
    // multiple tiles on one row. Both are left-padded uniformly so
    // the fresque reads as a stack of aligned sentences rather than a
    // centered monument.
    println!("    {line}");
}

fn render_split_band(hub_tiles: &[&Tile], editorial_tiles: &[&Tile], delta_leader: Option<&str>) {
    let hub_line = if hub_tiles.is_empty() {
        "   (vacant)".dimmed().to_string()
    } else {
        hub_tiles
            .iter()
            .map(|t| render_tile(t, delta_leader))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let editorial_line = if editorial_tiles.is_empty() {
        "   (vacant)".dimmed().to_string()
    } else {
        editorial_tiles
            .iter()
            .map(|t| render_tile(t, delta_leader))
            .collect::<Vec<_>>()
            .join("  ")
    };

    // The half-column split is hand-tuned for BAND_WIDTH=82.
    let half = BAND_WIDTH / 2;
    let hub_padded = left_pad_visible(&hub_line, half);
    println!("    {hub_padded} ║ {editorial_line}");
}

/// Pad the visible (ANSI-stripped) width of `s` to at least `width`
/// columns, appending spaces at the end.
fn left_pad_visible(s: &str, width: usize) -> String {
    let visible = visible_width(s);
    if visible >= width {
        s.to_string()
    } else {
        let pad: String = " ".repeat(width - visible);
        format!("{s}{pad}")
    }
}

/// Count visible terminal columns by stripping ANSI CSI sequences — the
/// `colored` crate wraps labels in `\x1b[...m...\x1b[0m`, which would
/// otherwise throw off the column arithmetic for the split band.
fn visible_width(s: &str) -> usize {
    let mut w = 0;
    let mut in_csi = false;
    for c in s.chars() {
        if in_csi {
            if c == 'm' {
                in_csi = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_csi = true;
            continue;
        }
        w += 1;
    }
    w
}

fn render_tile(t: &Tile, delta_leader: Option<&str>) -> String {
    let (lh, rh) = halo_brackets(t.success, t.measurement_ceiling);
    let label = colored_label(&t.name, &t.kind, t.health);
    // The red dot leads the tile so the eye catches it first.
    let prefix = if Some(t.name.as_str()) == delta_leader {
        "●".bright_red().bold().to_string()
    } else {
        String::new()
    };
    format!("{prefix}{lh}{label}{rh}")
}

fn render_legend(delta_leader: Option<&str>) {
    println!("{}", "Legend".bold());
    println!(
        "   {}   {}",
        "══[ thick halo ]══".bold(),
        "high North Star (success, log-scaled)".dimmed()
    );
    println!(
        "   {}   {}",
        "──[ medium ]──".bold(),
        "moderate North Star".dimmed()
    );
    println!(
        "   {}   {}",
        "··[ faint ]··".bold(),
        "low North Star".dimmed()
    );
    println!(
        "   {}   {}",
        "╌[ dashed ]╌".bold(),
        "measurement-ceiling — carnot's honesty (synthesis §6)".dimmed()
    );
    let delta_note = match delta_leader {
        Some(name) => format!("delta-of-day: {name}"),
        None => "delta-of-day: none (all galaxies within weekly variance)".to_string(),
    };
    println!("   {}   {}", "●".bright_red().bold(), delta_note.dimmed());
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_ordinal_is_canonical() {
        assert!(family_ordinal("infra") < family_ordinal("project"));
        assert!(family_ordinal("project") < family_ordinal("social-hub"));
        assert!(family_ordinal("social-hub") < family_ordinal("editorial"));
    }

    #[test]
    fn tile_width_floored_and_clamped() {
        assert!(tile_width_for(0) >= MIN_TILE);
        assert!(tile_width_for(10_000) <= MAX_TILE);
        // Log-scale: 100 commits should not blow past the clamp.
        assert!(tile_width_for(100) <= MAX_TILE);
    }

    #[test]
    fn measurement_ceiling_matches_synthesis_6() {
        // editorial, social-hub, project are ceiling-bounded.
        assert!(family_has_measurement_ceiling("editorial"));
        assert!(family_has_measurement_ceiling("social-hub"));
        assert!(family_has_measurement_ceiling("project"));
        // infra is directly observable — NOT ceiling-bounded.
        assert!(!family_has_measurement_ceiling("infra"));
    }

    #[test]
    fn halo_picks_dashed_for_ceiling_families() {
        let (l, _) = halo_brackets(3.0, true);
        assert!(l.contains('╌'));
        let (l, _) = halo_brackets(3.0, false);
        assert!(l.contains('═'));
    }

    #[test]
    fn health_score_is_bounded_per_family() {
        for kind in ["infra", "project", "social-hub", "editorial"] {
            for a7 in [0_u64, 1, 5, 20, 100] {
                for a1 in [0_u64, 1, 10] {
                    let h = health_score(kind, a7, a1);
                    assert!((0.0..=1.0).contains(&h), "{kind}/{a7}/{a1} → {h}");
                }
            }
        }
    }

    #[test]
    fn visible_width_strips_ansi() {
        let colored = format!("{}", "hello".red());
        assert_eq!(visible_width(&colored), 5);
        assert_eq!(visible_width("plain"), 5);
    }

    #[test]
    fn chunk_tiles_respects_per_row_cap() {
        // Build a throwaway tile list and verify the chunking used for
        // the PROJECT band wraps at 3 per row (the fresque's layout).
        let tiles: Vec<Tile> = (0..7)
            .map(|i| Tile {
                name: format!("g{i}"),
                kind: "project".into(),
                local_path: String::new(),
                activity_7d: 0,
                activity_1d: 0,
                tile_width: MIN_TILE,
                success: 0.0,
                health: 0.5,
                delta: 0.0,
                measurement_ceiling: true,
                is_delta_leader: false,
            })
            .collect();
        let refs: Vec<&Tile> = tiles.iter().collect();
        let rows = chunk_tiles(refs, 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].len(), 3);
        assert_eq!(rows[1].len(), 3);
        assert_eq!(rows[2].len(), 1);
    }

    #[test]
    fn render_fresque_no_panic_on_empty() {
        render_fresque(&[], None);
    }
}
