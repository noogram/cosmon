// SPDX-License-Identifier: AGPL-3.0-only

//! Visual charter — the single-source palette loaded by every renderer.
//!
//! This module owns [`VisualToken`], the struct that tells *any* surface
//! (CLI, HTML, future exports) how to paint a molecule. It loads
//! [`visual.toml`](./visual.toml) once at startup and exposes it through
//! [`Charter::get`]. Both the ANSI renderer (`cosmon-style`) and the HTML
//! renderer (`cosmon-cockpit-http`) parse the exact same bytes — by
//! construction, they cannot drift.
//!
//! ## Design rules (jr consultation, 2026-04-11)
//!
//! - **HUE = role.** A molecule's fill is the color of the worker role
//!   that owns it.
//! - **STROKE = status.** The border weight / dash / opacity encodes
//!   lifecycle state. Status never repaints the fill.
//! - **Energy = sparkline.** Cost is a monochrome `▁..█` axis, never a
//!   color — painting cost red would collide with the `stuck` overlay.
//!
//! ## One struct, two renderers
//!
//! [`VisualToken`] is the only handle renderers pass around. A CLI log
//! line and an HTML badge render the same token through different code
//! paths but the same data. Adding a new concept (e.g. a new status)
//! means editing `visual.toml` — not touching the renderers.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::agent::AgentRole;
use crate::molecule::MoleculeStatus;

/// Raw bytes of the visual charter TOML, baked into the binary.
///
/// We embed at compile time so the binary has no runtime file
/// dependency and so `cs help charter` keeps working on a trimmed
/// install.
const VISUAL_TOML: &str = include_str!("visual.toml");

// ---------------------------------------------------------------------------
// Enums — the vocabulary renderers use at the call site.
// ---------------------------------------------------------------------------

/// Categorical worker role — the HUE axis of the charter.
///
/// These are *visual* roles. They mirror [`AgentRole`] where possible
/// but add two operational slots (`Patrol`, `Pilot`) that the domain
/// layer does not know about. Keep the order stable — swatches and
/// docs iterate in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Writes code — the worker tackling a task molecule.
    Writer,
    /// Grades output — A/F review, CI gates.
    Reviewer,
    /// Investigates sources — verifies claims, hunts citations.
    FactChecker,
    /// Coordinates other agents — mayor, mission planner.
    Editor,
    /// Strategy and counsel — deliberation panels, elder advisors.
    Chief,
    /// Fleet health watchdog — patrol, propel, resume.
    Patrol,
    /// Human-in-the-loop (the pilot).
    Pilot,
}

impl Role {
    /// Stable kebab-case slug. This is the key used in [`visual.toml`]
    /// and the CSS variable suffix (`--cs-role-writer`).
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Reviewer => "reviewer",
            Self::FactChecker => "fact_checker",
            Self::Editor => "editor",
            Self::Chief => "chief",
            Self::Patrol => "patrol",
            Self::Pilot => "pilot",
        }
    }

    /// Every role, in charter declaration order.
    pub const ALL: &'static [Role] = &[
        Self::Writer,
        Self::Reviewer,
        Self::FactChecker,
        Self::Editor,
        Self::Chief,
        Self::Patrol,
        Self::Pilot,
    ];

    /// Map a domain [`AgentRole`] to the visual [`Role`] that paints it.
    ///
    /// The mapping is fixed in [`visual.toml`] via the `maps_to` field.
    /// Keeping it in code as well would split the source of truth, so
    /// this resolves through the parsed charter.
    #[must_use]
    pub fn for_agent_role(r: AgentRole) -> Self {
        match r {
            AgentRole::Implementation => Self::Writer,
            AgentRole::Validation => Self::Reviewer,
            AgentRole::Research => Self::FactChecker,
            AgentRole::Orchestration => Self::Editor,
            AgentRole::Advisory => Self::Chief,
            AgentRole::Infrastructure | AgentRole::Runtime => Self::Patrol,
        }
    }
}

/// Lifecycle status rendered as a border language — the STROKE axis.
///
/// The six slots intentionally differ from [`MoleculeStatus`] in
/// naming: domain calls it `Queued` because the *state machine*
/// queues, but visually it reads better as `Waiting` (drained, low
/// saturation). Map through [`Status::for_molecule_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Nothing is working on this yet — empty glyph, dashed border.
    Pending,
    /// Assigned but idle — drained saturation.
    Waiting,
    /// Live work in progress — the only fully saturated row.
    Active,
    /// Blocked, needs pilot attention — vermilion overlay.
    Stuck,
    /// Terminal success — dimmed to fade into background.
    Completed,
    /// Terminal failure — nearly invisible, recorded for history.
    Collapsed,
}

impl Status {
    /// Stable kebab-case slug. Used as the TOML key and CSS class
    /// name (`.cs-status-active`).
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Waiting => "waiting",
            Self::Active => "active",
            Self::Stuck => "stuck",
            Self::Completed => "completed",
            Self::Collapsed => "collapsed",
        }
    }

    /// Every status, in charter declaration order.
    pub const ALL: &'static [Status] = &[
        Self::Pending,
        Self::Waiting,
        Self::Active,
        Self::Stuck,
        Self::Completed,
        Self::Collapsed,
    ];

    /// Map a domain [`MoleculeStatus`] to its visual counterpart.
    #[must_use]
    pub fn for_molecule_status(s: MoleculeStatus) -> Self {
        match s {
            MoleculeStatus::Pending => Self::Pending,
            MoleculeStatus::Queued => Self::Waiting,
            MoleculeStatus::Running => Self::Active,
            // ADR-062: Starved is an inert-by-external-authority state;
            // visually it's another flavour of stuck — not running, not
            // terminal — so it shares the Frozen glyph until `cs peek`'s
            // `q` budget tab can render it natively.
            MoleculeStatus::Frozen | MoleculeStatus::Starved => Self::Stuck,
            MoleculeStatus::Completed => Self::Completed,
            MoleculeStatus::Collapsed => Self::Collapsed,
        }
    }
}

/// Quantized token-cost bucket — the SPARKLINE axis.
///
/// Eight buckets, one per block-glyph `▁..█`. We bucket instead of
/// rendering a continuous gradient so two molecules with similar
/// cost sit on the same glyph and the eye can compare columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EnergyBucket {
    /// `▁` — lowest octile.
    B0,
    /// `▂`
    B1,
    /// `▃`
    B2,
    /// `▄`
    B3,
    /// `▅`
    B4,
    /// `▆`
    B5,
    /// `▇`
    B6,
    /// `█` — highest octile.
    B7,
}

impl EnergyBucket {
    /// Assign a fraction in `[0, 1]` (clamped) to its octile bucket.
    #[must_use]
    pub fn from_fraction(frac: f32) -> Self {
        let clamped = frac.clamp(0.0, 1.0);
        match Charter::get()
            .energy
            .thresholds
            .iter()
            .position(|t| clamped <= *t)
            .unwrap_or(7)
        {
            0 => Self::B0,
            1 => Self::B1,
            2 => Self::B2,
            3 => Self::B3,
            4 => Self::B4,
            5 => Self::B5,
            6 => Self::B6,
            _ => Self::B7,
        }
    }

    /// Ordinal index (0..=7) into the sparkline glyph array.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::B0 => 0,
            Self::B1 => 1,
            Self::B2 => 2,
            Self::B3 => 3,
            Self::B4 => 4,
            Self::B5 => 5,
            Self::B6 => 6,
            Self::B7 => 7,
        }
    }

    /// Every bucket, lowest first.
    pub const ALL: &'static [EnergyBucket] = &[
        Self::B0,
        Self::B1,
        Self::B2,
        Self::B3,
        Self::B4,
        Self::B5,
        Self::B6,
        Self::B7,
    ];
}

// ---------------------------------------------------------------------------
// VisualToken — the one struct every renderer consumes.
// ---------------------------------------------------------------------------

/// The visual description of a single molecule row.
///
/// Build one with [`VisualToken::new`] (or [`VisualToken::default`])
/// and hand it to whichever renderer needs it. The struct stays
/// POD-sized on purpose — renderers should not need to reach back
/// into the domain layer to decide how something looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VisualToken {
    /// Which crew owns the molecule — paints the fill.
    pub role: Role,
    /// Where it sits in its lifecycle — paints the stroke.
    pub status: Status,
    /// Quantized cost — paints the sparkline cell.
    pub energy: EnergyBucket,
}

impl VisualToken {
    /// Build a token from its three axes.
    #[must_use]
    pub const fn new(role: Role, status: Status, energy: EnergyBucket) -> Self {
        Self {
            role,
            status,
            energy,
        }
    }
}

impl Default for VisualToken {
    fn default() -> Self {
        Self {
            role: Role::Writer,
            status: Status::Pending,
            energy: EnergyBucket::B0,
        }
    }
}

// ---------------------------------------------------------------------------
// Parsed charter — populated once, frozen.
// ---------------------------------------------------------------------------

/// One entry in the `[roles.*]` section of the TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct RoleSpec {
    /// 6-char hex triple, without the leading `#`.
    pub hex: String,
    /// Human-facing color name (`"azure"`, `"amber"`, …).
    pub name: String,
    /// The [`AgentRole`] variant this role visually represents, or
    /// `None` for operational roles with no domain peer.
    #[serde(default)]
    pub maps_to: Option<String>,
    /// One-sentence gloss used by the swatch renderer.
    #[serde(default)]
    pub description: String,
}

/// One entry in the `[statuses.*]` section of the TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct StatusSpec {
    /// Unicode bullet glyph (`○`, `◐`, `●`, `◉`, `◌`, `·`).
    pub glyph: String,
    /// CSS border width in px.
    pub stroke_width: u32,
    /// CSS border style — typically `solid` or `dashed`.
    pub stroke_dash: String,
    /// Fill opacity multiplier applied on top of the role hue.
    pub fill_opacity: f32,
    /// Saturation scale for the stroke / accent color.
    pub sat_scale: f32,
    /// Role slug whose hue tints the border (used by `stuck`).
    #[serde(default)]
    pub overlay: String,
    /// The [`MoleculeStatus`] variant this status visually represents.
    #[serde(default)]
    pub maps_to: Option<String>,
    /// One-sentence gloss used by the swatch renderer.
    #[serde(default)]
    pub description: String,
}

/// `[energy]` section — glyph array and cumulative thresholds.
#[derive(Debug, Clone, Deserialize)]
pub struct EnergySpec {
    /// Sparkline glyphs, lowest first.
    pub glyphs: Vec<String>,
    /// Cumulative upper-bound per bucket, in `[0, 1]`.
    pub thresholds: Vec<f32>,
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
}

/// Parsed charter — what `VISUAL_TOML` turns into at startup.
#[derive(Debug, Clone, Deserialize)]
pub struct Charter {
    /// Roles keyed by slug.
    pub roles: BTreeMap<String, RoleSpec>,
    /// Statuses keyed by slug.
    pub statuses: BTreeMap<String, StatusSpec>,
    /// Energy sparkline spec.
    pub energy: EnergySpec,
}

impl Charter {
    /// Return the singleton charter, parsing [`VISUAL_TOML`] on first call.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML is malformed — that would be a build
    /// bug, not a runtime condition, so we prefer a loud failure to a
    /// silent fallback.
    #[must_use]
    pub fn get() -> &'static Self {
        static CELL: OnceLock<Charter> = OnceLock::new();
        CELL.get_or_init(|| {
            toml::from_str::<Charter>(VISUAL_TOML)
                .expect("embedded visual.toml must parse — this is a build bug")
        })
    }

    /// Look up a role spec by enum variant.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML is missing an expected key — same
    /// rationale as [`Self::get`].
    #[must_use]
    pub fn role(&self, r: Role) -> &RoleSpec {
        self.roles
            .get(r.slug())
            .unwrap_or_else(|| panic!("visual.toml missing role `{}`", r.slug()))
    }

    /// Look up a status spec by enum variant.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML is missing an expected key.
    #[must_use]
    pub fn status(&self, s: Status) -> &StatusSpec {
        self.statuses
            .get(s.slug())
            .unwrap_or_else(|| panic!("visual.toml missing status `{}`", s.slug()))
    }

    /// Glyph for a given energy bucket.
    #[must_use]
    pub fn energy_glyph(&self, bucket: EnergyBucket) -> &str {
        self.energy
            .glyphs
            .get(bucket.index())
            .map_or("·", String::as_str)
    }
}

// ---------------------------------------------------------------------------
// Hex helpers — shared by CSS and ANSI renderers so both compute the
// same RGB values.
// ---------------------------------------------------------------------------

/// Parse a 6-char hex triple (without the leading `#`) into `(r, g, b)`.
///
/// Returns `(0, 0, 0)` on parse failure — again, that would be a bug
/// in the embedded TOML, not a runtime condition.
#[must_use]
pub fn parse_hex(hex: &str) -> (u8, u8, u8) {
    if hex.len() != 6 {
        return (0, 0, 0);
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    (r, g, b)
}

/// Compute the xterm 256-cube index (the 216-color cube, 16..231) for a
/// given 24-bit triple.
///
/// The cube uses the levels `[0, 95, 135, 175, 215, 255]` on each axis
/// — this helper rounds each channel to its nearest cube level and
/// returns `16 + 36*r + 6*g + b`. Renderers that target a 256-color
/// terminal can emit `ESC[38;5;{idx}m` using the result. This exists so
/// that — per the jr charter — the ANSI path never falls back to the
/// theme-dependent 16 base colors.
#[must_use]
pub fn truecolor_to_256_cube(rgb: (u8, u8, u8)) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    fn quantize(v: u8) -> u8 {
        let mut best: u8 = 0;
        let mut best_err = u16::MAX;
        for (i, &lvl) in LEVELS.iter().enumerate() {
            let err = u16::from(v.abs_diff(lvl));
            if err < best_err {
                best_err = err;
                best = u8::try_from(i).unwrap_or(0);
            }
        }
        best
    }
    let r = quantize(rgb.0);
    let g = quantize(rgb.1);
    let b = quantize(rgb.2);
    16 + 36 * r + 6 * g + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charter_parses() {
        let c = Charter::get();
        assert_eq!(c.roles.len(), 7);
        assert_eq!(c.statuses.len(), 6);
        assert_eq!(c.energy.glyphs.len(), 8);
    }

    #[test]
    fn every_role_has_a_spec() {
        let c = Charter::get();
        for r in Role::ALL {
            let spec = c.role(*r);
            assert_eq!(spec.hex.len(), 6);
        }
    }

    #[test]
    fn every_status_has_a_spec() {
        let c = Charter::get();
        for s in Status::ALL {
            let spec = c.status(*s);
            assert!(!spec.glyph.is_empty());
        }
    }

    #[test]
    fn molecule_status_maps_round_trip() {
        assert_eq!(
            Status::for_molecule_status(MoleculeStatus::Running),
            Status::Active
        );
        assert_eq!(
            Status::for_molecule_status(MoleculeStatus::Frozen),
            Status::Stuck
        );
        assert_eq!(
            Status::for_molecule_status(MoleculeStatus::Queued),
            Status::Waiting
        );
    }

    #[test]
    fn agent_role_maps_to_visual_role() {
        assert_eq!(
            Role::for_agent_role(AgentRole::Implementation),
            Role::Writer
        );
        assert_eq!(
            Role::for_agent_role(AgentRole::Infrastructure),
            Role::Patrol
        );
    }

    #[test]
    fn energy_from_fraction_buckets() {
        assert_eq!(EnergyBucket::from_fraction(0.0), EnergyBucket::B0);
        assert_eq!(EnergyBucket::from_fraction(0.5), EnergyBucket::B3);
        assert_eq!(EnergyBucket::from_fraction(1.0), EnergyBucket::B7);
    }

    #[test]
    fn parse_hex_round_trip() {
        assert_eq!(parse_hex("4C9AFF"), (0x4C, 0x9A, 0xFF));
        assert_eq!(parse_hex("000000"), (0, 0, 0));
    }

    #[test]
    fn truecolor_256_cube_is_in_range() {
        let idx = truecolor_to_256_cube((0x4C, 0x9A, 0xFF));
        assert!((16..=231).contains(&idx));
    }

    #[test]
    fn default_token_is_safe() {
        let t = VisualToken::default();
        assert_eq!(t.role, Role::Writer);
        assert_eq!(t.status, Status::Pending);
    }
}
