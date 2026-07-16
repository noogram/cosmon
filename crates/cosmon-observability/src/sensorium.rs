// SPDX-License-Identifier: AGPL-3.0-only

//! Sensorium — five-organ vital-strip projection.
//!
//! The [`Sensorium`] is a pure, render-time aggregation of the five
//! cosmon-incarné organs (peau, cœur, visage, carnet, voix) plus the
//! `~/.cosmon/autopilot.off` kill-switch. It is the data shape consumed
//! by [`crate::render::render_vital_strip`] to emit the single fixed-width
//! ASCII line wedged between `cs peek --snapshot`'s header and its
//! molecule list.
//!
//! Per `ADR-109 (sensorium-strip)`, the strip alphabet is
//! immutable for v0:
//!
//! ```text
//! ~ 03  . . . * . . . * . .   @ democorp/cosmon   = 4.2k notes   > 1 awaiting
//! ```
//!
//! - `~` peau (channels-in) — unhandled signals in the last 24h.
//! - `. / *` cœur (heartbeat) — last ten beats; `.` ticked, `*` moved a
//!   molecule.
//! - `@` visage (identity) — galaxy whose `SOUL.md` is in scope; a
//!   trailing `!` flags seal-drift.
//! - `=` carnet (memory) — durable note count; `-` decay marks an
//!   announced forgetting.
//! - `>` voix (channels-out) — drafts awaiting operator permission.
//! - `[off]` — the kill-switch made visible.
//!
//! This crate intentionally does **not** load the strip from disk —
//! filesystem I/O lives in `cosmon-cli` (the loader walks
//! `.cosmon/state/sensorium/`). The boundary is the same one
//! [`crate::aggregate::FleetSnapshot`] already enforces.

use serde::{Deserialize, Serialize};

/// Number of heartbeats the strip renders as `. / *` glyphs.
///
/// Ten is the alphabet width on which the panel converged
/// (`responses/jr.md`); changing it is a breaking change for any
/// menubar viewport or golden-snapshot consumer.
pub const HEARTBEAT_WINDOW: usize = 10;

/// State of a single heartbeat slot in the strip window.
///
/// - [`HeartbeatKind::Resting`] (`.`) — beat landed, no molecule moved.
/// - [`HeartbeatKind::Live`] (`*`) — beat landed and moved at least one
///   molecule.
/// - [`HeartbeatKind::Missed`] (` `) — no beat at all in this slot.
///
/// A row of all dots, no stars, for a week is the cœur saying *"I tick
/// but nothing happens"*. Stillness is the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeartbeatKind {
    /// `.` — beat landed, no molecule moved.
    Resting,
    /// `*` — beat landed and moved at least one molecule.
    Live,
    /// ` ` — no beat at all in this slot (oldest slots when fewer than
    /// [`HEARTBEAT_WINDOW`] beats have been recorded).
    Missed,
}

impl HeartbeatKind {
    /// One-byte ASCII rendering of the heartbeat slot.
    #[must_use]
    pub const fn glyph(self) -> char {
        match self {
            Self::Resting => '.',
            Self::Live => '*',
            Self::Missed => ' ',
        }
    }
}

/// Aggregate of the five organs for one strip render.
///
/// The values are all derived from the byte-identical-when-unchanged
/// `.cosmon/state/sensorium/` source files; absence is `0` / `None`,
/// never an error. See [`Self::is_empty`] for the canonical
/// no-state baseline that produces the all-zero strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sensorium {
    /// `~ NN` — peau (channels-in). Count of unhandled signals landed in
    /// the last 24h, read from `sensorium/inbox.ndjson`. Capped at 99 in
    /// the rendered strip (two-digit field).
    pub peau_signals_24h: u32,

    /// `. . . *` — cœur (heartbeat). The last [`HEARTBEAT_WINDOW`]
    /// beats, oldest-first. Read from `sensorium/heartbeat.ndjson`;
    /// rows with non-empty `moved` arrays become [`HeartbeatKind::Live`].
    pub heartbeat: [HeartbeatKind; HEARTBEAT_WINDOW],

    /// `@ <galaxy>` — visage (identity). The galaxy whose `SOUL.md` is
    /// in scope, read from frontmatter `name:`. Absent when no SOUL is
    /// loaded; the strip then prints the literal `<galaxy>` placeholder.
    pub visage_galaxy: Option<String>,

    /// Trailing `!` after the galaxy name to flag a BLAKE3 seal
    /// mismatch on `SOUL.md`. Briefing-seal discipline (cosmon CLAUDE.md
    /// §"Briefing seals") applied to identity: the strip never blocks,
    /// it surfaces drift.
    pub visage_seal_drift: bool,

    /// `= NNN notes` — carnet (memory). Count of durable notes under
    /// `sensorium/notes/*.md`.
    pub carnet_count: u64,

    /// `-NN in 6h` — carnet decay (memory). Number of notes whose
    /// `decay_at:` frontmatter falls within the next 6 hours.
    /// `None` when no notes are within the decay horizon — the strip
    /// then omits the decay glyph entirely. Forgetting is *announced*,
    /// not silent.
    pub carnet_decay_6h: Option<u32>,

    /// `> N awaiting` — voix (channels-out). Drafts under
    /// `sensorium/outbox/*.md` whose frontmatter has
    /// `permission: pending`. Always `0` in v0 — the column exists so
    /// the lit-up future is visible before it ships.
    pub voix_awaiting: u32,

    /// `[off]` — kill-switch visible. True when `~/.cosmon/autopilot.off`
    /// exists. Organs still tick on disk; only the rendering dims.
    /// Silence is the *guarantee* of the kill-switch, not its
    /// consequence.
    pub autopilot_off: bool,
}

impl Default for Sensorium {
    fn default() -> Self {
        // Heartbeat default is [Resting; 10] — the "no state has been
        // written" canonical baseline reads `. . . . . . . . . .` per
        // the briefing example (delib-20260521-955f §5.3). The
        // [`HeartbeatKind::Missed`] variant is reserved for slots that
        // explicitly carry a missed-beat signal in the loaded
        // heartbeat history; it is never the default.
        Self {
            peau_signals_24h: 0,
            heartbeat: [HeartbeatKind::Resting; HEARTBEAT_WINDOW],
            visage_galaxy: None,
            visage_seal_drift: false,
            carnet_count: 0,
            carnet_decay_6h: None,
            voix_awaiting: 0,
            autopilot_off: false,
        }
    }
}

impl Sensorium {
    /// `true` iff every organ is at its zero baseline — no signals, no
    /// live beats, no carnet, no outbox, kill-switch off, no
    /// seal-drift, no galaxy. This is the state the strip renders as
    /// `~ 00  . . . . . . . . . .   @ <galaxy>   = 0 notes   > 0 awaiting`
    /// per the briefing example.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peau_signals_24h == 0
            && self.voix_awaiting == 0
            && self.carnet_count == 0
            && self.carnet_decay_6h.is_none()
            && self.visage_galaxy.is_none()
            && !self.visage_seal_drift
            && !self.autopilot_off
            && self
                .heartbeat
                .iter()
                .all(|h| matches!(h, HeartbeatKind::Resting))
    }

    /// JSON projection of the sensorium — the wire shape of
    /// `cs sensorium --json` (ADR-068 UX↔CLI parity).
    ///
    /// Stable keys: `peau`, `coeur`, `visage`, `carnet`, `voix`,
    /// `autopilot_off`. Every viewport that wants to re-render the
    /// strip without parsing ASCII reads this JSON.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let beats: Vec<&str> = self
            .heartbeat
            .iter()
            .map(|h| match h {
                HeartbeatKind::Resting => "resting",
                HeartbeatKind::Live => "live",
                HeartbeatKind::Missed => "missed",
            })
            .collect();
        serde_json::json!({
            "peau": {
                "signals_24h": self.peau_signals_24h,
            },
            "coeur": {
                "beats": beats,
            },
            "visage": {
                "galaxy": self.visage_galaxy,
                "seal_drift": self.visage_seal_drift,
            },
            "carnet": {
                "count": self.carnet_count,
                "decay_6h": self.carnet_decay_6h,
            },
            "voix": {
                "awaiting": self.voix_awaiting,
            },
            "autopilot_off": self.autopilot_off,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let s = Sensorium::default();
        assert!(s.is_empty());
        assert_eq!(s.peau_signals_24h, 0);
        assert!(s.visage_galaxy.is_none());
    }

    #[test]
    fn heartbeat_glyphs_are_seven_bit_ascii() {
        for kind in [
            HeartbeatKind::Resting,
            HeartbeatKind::Live,
            HeartbeatKind::Missed,
        ] {
            let g = kind.glyph();
            assert!(g.is_ascii(), "{kind:?} glyph not ASCII");
        }
    }

    #[test]
    fn to_json_has_stable_top_keys() {
        let v = Sensorium::default().to_json();
        for key in ["peau", "coeur", "visage", "carnet", "voix", "autopilot_off"] {
            assert!(v.get(key).is_some(), "missing key {key}");
        }
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn presence_of_any_organ_breaks_is_empty() {
        let mut s = Sensorium::default();
        s.peau_signals_24h = 1;
        assert!(!s.is_empty());

        let mut s = Sensorium::default();
        s.heartbeat[0] = HeartbeatKind::Live;
        assert!(!s.is_empty());

        let mut s = Sensorium::default();
        s.visage_galaxy = Some("cosmon".into());
        assert!(!s.is_empty());

        let mut s = Sensorium::default();
        s.autopilot_off = true;
        assert!(!s.is_empty());
    }
}
