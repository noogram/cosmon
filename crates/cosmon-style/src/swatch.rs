// SPDX-License-Identifier: AGPL-3.0-only

//! `cs help charter` swatch — renders the whole charter so operators
//! can see exactly what their terminal will print.
//!
//! The swatch prints five sections: roles (hue axis), statuses
//! (stroke glyphs), the cartesian product of role × status at a
//! glance, the energy sparkline, and a short legend for the three
//! axes. It is deliberately ASCII-terminal friendly — no boxes, no
//! tables, just lines.

use std::fmt::Write;

use colored::Colorize;
use cosmon_core::visual::{Charter, EnergyBucket, Role, Status, VisualToken};

use crate::ansi::{format_token, paint_hex};

/// Render the swatch as a `String`. Testable and redirect-friendly.
#[must_use]
pub fn render_swatch() -> String {
    let charter = Charter::get();
    let mut out = String::with_capacity(4096);

    section(&mut out, "Roles (HUE axis)");
    for r in Role::ALL {
        let spec = charter.role(*r);
        let swatch = paint_hex("  ██  ", &spec.hex);
        let _ = writeln!(
            out,
            "{swatch} {:<14} #{:<8} {}",
            spec.name, spec.hex, spec.description
        );
    }

    section(&mut out, "Statuses (STROKE axis)");
    for s in Status::ALL {
        let spec = charter.status(*s);
        // Every row in this section uses the writer hue so the eye
        // reads status differences (glyph, overlay) against a fixed
        // fill. The role hue is already demoed in the previous
        // section and the matrix below.
        let tok = VisualToken::new(Role::Writer, *s, EnergyBucket::B0);
        let painted = format_token(tok);
        let _ = writeln!(out, "  {painted:<24} {}", spec.description);
    }

    section(&mut out, "Role × Status matrix");
    for r in Role::ALL {
        let spec = charter.role(*r);
        let _ = write!(out, "  {:<14}", spec.name);
        for s in Status::ALL {
            let tok = VisualToken::new(*r, *s, EnergyBucket::B0);
            let cell = format_token(tok);
            let _ = write!(out, " {}", truncate(&format!("{cell}"), 14));
        }
        out.push('\n');
    }

    section(&mut out, "Energy sparkline");
    let mut line = String::from("  ");
    for b in EnergyBucket::ALL {
        line.push_str(charter.energy_glyph(*b));
        line.push(' ');
    }
    out.push_str(&line);
    out.push('\n');
    out.push_str("  cheap → expensive (no color axis — see charter rationale)\n");

    section(&mut out, "Legend");
    out.push_str("  HUE    = role (who owns the molecule)\n");
    out.push_str("  STROKE = status (where it sits in its lifecycle)\n");
    out.push_str("  SPARK  = energy bucket (how much it costs)\n");

    out
}

/// Print the swatch to stdout. Thin wrapper — `cs help charter` does
/// not need to import `std::fmt::Write` to echo a `String`.
pub fn print_swatch() {
    print!("{}", render_swatch());
}

fn section(out: &mut String, title: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(out, "{}", title.bold());
}

/// Best-effort truncate for the matrix cells. Because `ColoredString`
/// wraps ANSI escapes around the visible text, a naive `char_indices`
/// truncate would cut an escape code in half — so we just return the
/// full string and let terminals wrap. The width parameter is kept
/// for future tuning.
fn truncate(s: &str, _width: usize) -> String {
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swatch_mentions_every_section() {
        let s = render_swatch();
        for needle in [
            "Roles (HUE axis)",
            "Statuses (STROKE axis)",
            "Role × Status matrix",
            "Energy sparkline",
            "Legend",
        ] {
            assert!(s.contains(needle), "missing: {needle}");
        }
    }

    #[test]
    fn swatch_mentions_every_role_name() {
        let s = render_swatch();
        for r in Role::ALL {
            let spec = Charter::get().role(*r);
            assert!(s.contains(&spec.name), "missing role name: {}", spec.name);
        }
    }

    #[test]
    fn swatch_mentions_every_status_slug() {
        let s = render_swatch();
        for st in Status::ALL {
            assert!(s.contains(st.slug()), "missing status: {}", st.slug());
        }
    }

    #[test]
    fn swatch_mentions_every_energy_glyph() {
        let s = render_swatch();
        for g in &Charter::get().energy.glyphs {
            assert!(s.contains(g.as_str()), "missing glyph: {g}");
        }
    }
}
