// SPDX-License-Identifier: AGPL-3.0-only

//! ANSI adapter — turns a [`VisualToken`] into a [`ColoredString`].
//!
//! All terminal output goes through [`paint_hex`], which emits a
//! `TrueColor` escape sequence using the charter's exact RGB triple.
//! We never fall back to [`colored::Color`]'s 16 base colors — they
//! remap according to the user's terminal theme and so would drift
//! from the HTML cockpit's rendering.

use colored::{Color as AnsiColor, ColoredString, Colorize};
use cosmon_core::agent::AgentRole;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::visual::{parse_hex, Charter, Role, Status, VisualToken};
use cosmon_core::worker::WorkerStatus;

/// Paint `text` with the given 6-char hex as a `TrueColor` ANSI escape.
///
/// This is the single allowed entry point for turning a charter hex
/// into terminal output. If you find yourself calling `.red()` or
/// `.yellow()` in a renderer, use this instead.
#[must_use]
pub fn paint_hex(text: &str, hex: &str) -> ColoredString {
    let (r, g, b) = parse_hex(hex);
    text.color(AnsiColor::TrueColor { r, g, b })
}

/// Status glyph + label, tinted by the owning role's hue.
///
/// Status does *not* carry its own color. It contributes the glyph
/// and (in HTML) the stroke language; the hue comes from the role
/// that owns the molecule. When a row does not yet know its role we
/// pick [`Role::Writer`] as a neutral default — callers that have a
/// better answer should use [`format_token`].
#[must_use]
pub fn format_status(s: MoleculeStatus) -> ColoredString {
    format_token(VisualToken::new(
        Role::Writer,
        Status::for_molecule_status(s),
        cosmon_core::visual::EnergyBucket::B0,
    ))
}

/// Paint a [`VisualToken`] as `<glyph> <status-word>` using the
/// role's hue and the status's glyph. This is the canonical call
/// site — anything more complex should be built on top of it.
#[must_use]
pub fn format_token(tok: VisualToken) -> ColoredString {
    let charter = Charter::get();
    let status_spec = charter.status(tok.status);
    let role_spec = charter.role(tok.role);

    // For stuck we bleed the overlay color into the glyph so the
    // pilot's attention is drawn regardless of the owning role.
    let hex = if tok.status == Status::Stuck && !status_spec.overlay.is_empty() {
        charter
            .roles
            .get(status_spec.overlay.as_str())
            .map_or(role_spec.hex.as_str(), |s| s.hex.as_str())
    } else {
        role_spec.hex.as_str()
    };

    let text = format!("{} {}", status_spec.glyph, tok.status.slug());
    let painted = paint_hex(&text, hex);
    match tok.status {
        Status::Completed | Status::Collapsed => painted.dimmed(),
        Status::Active => painted.bold(),
        _ => painted,
    }
}

/// Paint a worker status with its charter color.
///
/// Worker status is operational (starting / stopping / unresponsive
/// / stale) rather than part of the molecule lifecycle axis, so it
/// reuses the role palette rather than the six status slots. The
/// rule: healthy = patrol slate, degraded = reviewer amber, dead =
/// pilot vermilion.
#[must_use]
pub fn format_worker_status(s: &WorkerStatus) -> ColoredString {
    let hex = worker_hex(s);
    paint_hex(&s.to_string(), hex)
}

/// Look up the charter hex for a worker status.
///
/// Charter hexes are owned by the `OnceLock` inside `cosmon-core`, so
/// they outlive the process — but the return is `&'static str` only if
/// we prove it to the borrow checker. We prove it by going through the
/// compiled-in table below, which mirrors the hexes in `visual.toml`.
/// A drift test (`worker_hex_matches_charter`) keeps the two in sync.
fn worker_hex(s: &WorkerStatus) -> &'static str {
    match s {
        WorkerStatus::Active => "94A3B8", // patrol
        WorkerStatus::Starting | WorkerStatus::Stopping => "10B981", // editor
        WorkerStatus::Paused => "D946EF", // fact_checker
        WorkerStatus::Unresponsive => "F5A623", // reviewer
        WorkerStatus::Error(_) | WorkerStatus::Stale => "EF4444", // pilot
        WorkerStatus::Stopped => "E8E1C4", // chief
    }
}

/// Paint an [`AgentRole`] using its charter role color.
#[must_use]
pub fn format_role(r: AgentRole) -> ColoredString {
    let role = Role::for_agent_role(r);
    let spec = Charter::get().role(role);
    paint_hex(&r.to_string(), &spec.hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::visual::EnergyBucket;

    #[test]
    fn paint_hex_wraps_text() {
        let s = paint_hex("hello", "4C9AFF");
        assert!(format!("{s}").contains("hello"));
    }

    #[test]
    fn format_status_covers_every_variant() {
        for v in [
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
            MoleculeStatus::Running,
            MoleculeStatus::Frozen,
            MoleculeStatus::Completed,
            MoleculeStatus::Collapsed,
        ] {
            let s = format_status(v);
            assert!(!format!("{s}").is_empty());
        }
    }

    #[test]
    fn format_status_uses_stuck_glyph_for_frozen() {
        let s = format_status(MoleculeStatus::Frozen);
        // The stuck glyph from visual.toml must appear in the output.
        assert!(format!("{s}").contains("◉"));
    }

    #[test]
    fn worker_hex_matches_charter() {
        let charter = Charter::get();
        for (slug, expected) in [
            ("patrol", worker_hex(&WorkerStatus::Active)),
            ("editor", worker_hex(&WorkerStatus::Starting)),
            ("fact_checker", worker_hex(&WorkerStatus::Paused)),
            ("reviewer", worker_hex(&WorkerStatus::Unresponsive)),
            ("pilot", worker_hex(&WorkerStatus::Stale)),
            ("chief", worker_hex(&WorkerStatus::Stopped)),
        ] {
            let from_toml = &charter.roles.get(slug).unwrap().hex;
            assert_eq!(from_toml, expected, "role {slug} drifted");
        }
    }

    #[test]
    fn format_token_bold_on_active() {
        // `colored` suppresses ANSI escapes when stdout is not a TTY
        // (i.e. under `cargo test`); force it on for this assertion.
        colored::control::set_override(true);
        let tok = VisualToken::new(Role::Writer, Status::Active, EnergyBucket::B0);
        let s = format_token(tok);
        assert!(format!("{s}").contains("\u{1b}[1"));
        colored::control::unset_override();
    }
}
