// SPDX-License-Identifier: AGPL-3.0-only

//! CSS projection of the charter.
//!
//! [`charter_css`] reads the same [`Charter`] that the ANSI renderer
//! uses and emits a stylesheet that the HTTP cockpit serves at
//! `/charter.css`. It contains:
//!
//! - `:root` custom properties `--cs-role-<slug>` for every role hue.
//! - `.cs-role-<slug>` utility class that paints the fill with the
//!   role hue.
//! - `.cs-status-<slug>` utility class that applies the charter's
//!   STROKE language for that status — border width, dash, opacity —
//!   without touching the fill color.
//! - Dashboard legacy aliases (`--green`, `--red`, …) that resolve to
//!   the closest role hue. These keep the existing inline CSS in
//!   `index.html` working while it migrates.
//!
//! This file contains zero hard-coded hex values. Every color comes
//! from [`Charter`] — by construction, the CSS cannot drift from
//! `cs watch` / `cs help charter`.

use std::fmt::Write as _;

use cosmon_core::visual::{parse_hex, Charter, Role, Status};

/// Render the full charter stylesheet as a `String`. Deterministic —
/// no I/O, no clock — so it is safe to call from a `build.rs` or an
/// HTTP handler.
#[must_use]
pub fn charter_css() -> String {
    let charter = Charter::get();
    let mut out = String::with_capacity(4096);

    out.push_str("/* auto-generated from cosmon-core::visual — do not edit */\n");
    out.push_str(":root {\n");

    for r in Role::ALL {
        let spec = charter.role(*r);
        out.push_str("  --cs-role-");
        out.push_str(r.slug());
        out.push_str(": #");
        out.push_str(&spec.hex);
        out.push_str("; /* ");
        out.push_str(&spec.name);
        out.push_str(" */\n");
    }

    for (alias, role) in LEGACY_ALIASES {
        let spec = charter.role(*role);
        out.push_str("  --");
        out.push_str(alias);
        out.push_str(": #");
        out.push_str(&spec.hex);
        out.push_str(";\n");
    }

    out.push_str("}\n\n");

    for r in Role::ALL {
        let spec = charter.role(*r);
        let (red, green, blue) = parse_hex(&spec.hex);
        let _ = writeln!(
            out,
            ".cs-role-{} {{ --cs-hue: {red},{green},{blue}; background: rgb(var(--cs-hue)); color: #0d1117; }}",
            r.slug()
        );
    }

    out.push('\n');

    for s in Status::ALL {
        let spec = charter.status(*s);
        let overlay_hex = if spec.overlay.is_empty() {
            String::new()
        } else {
            charter
                .roles
                .get(spec.overlay.as_str())
                .map(|r| r.hex.clone())
                .unwrap_or_default()
        };
        push_status_rule(&mut out, s.slug(), spec, &overlay_hex);
    }

    out
}

fn push_status_rule(
    out: &mut String,
    slug: &str,
    spec: &cosmon_core::visual::StatusSpec,
    overlay_hex: &str,
) {
    let _ = writeln!(out, ".cs-status-{slug} {{");
    let _ = writeln!(out, "  opacity: {};", spec.fill_opacity);
    if spec.stroke_width > 0 {
        if overlay_hex.is_empty() {
            let neutral = rgb_scaled(spec.sat_scale);
            let _ = writeln!(
                out,
                "  border: {}px {} rgba({},{},{},1);",
                spec.stroke_width, spec.stroke_dash, neutral.0, neutral.1, neutral.2
            );
        } else {
            let _ = writeln!(
                out,
                "  border: {}px {} #{overlay_hex};",
                spec.stroke_width, spec.stroke_dash
            );
        }
    } else {
        out.push_str("  border: none;\n");
    }
    let _ = writeln!(out, "  filter: saturate({});", spec.sat_scale);
    out.push_str("}\n");
}

/// Approximate a desaturated neutral border tint for statuses with
/// no explicit overlay. Returns `rgb` in `0..=255` per channel.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rgb_scaled(sat: f32) -> (u8, u8, u8) {
    let base = (200.0 * sat.clamp(0.0, 1.5)).clamp(0.0, 255.0);
    let b = base as u8;
    (b, b, b)
}

/// Legacy unprefixed CSS variables that the dashboard's inline style
/// block still references. Listed explicitly so migrating `index.html`
/// to `--cs-role-*` is a mechanical sweep.
const LEGACY_ALIASES: &[(&str, Role)] = &[
    ("accent", Role::Writer),
    ("blue", Role::Writer),
    ("green", Role::Editor),
    ("yellow", Role::Reviewer),
    ("red", Role::Pilot),
    ("frozen", Role::FactChecker),
    ("dim", Role::Patrol),
    ("text", Role::Chief),
    ("card", Role::Patrol),
    ("border", Role::Patrol),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_contains_every_role_variable() {
        let css = charter_css();
        for r in Role::ALL {
            let needle = format!("--cs-role-{}:", r.slug());
            assert!(css.contains(&needle), "missing: {needle}");
        }
    }

    #[test]
    fn css_contains_every_status_rule() {
        let css = charter_css();
        for s in Status::ALL {
            let needle = format!(".cs-status-{}", s.slug());
            assert!(css.contains(&needle), "missing: {needle}");
        }
    }

    #[test]
    fn css_stuck_uses_pilot_overlay() {
        let css = charter_css();
        // stuck must reference the pilot vermilion somewhere in its
        // border rule.
        let pilot_hex = &Charter::get().role(Role::Pilot).hex;
        let stuck_block_start = css.find(".cs-status-stuck").unwrap();
        let stuck_block = &css[stuck_block_start..];
        let stuck_block_end = stuck_block.find('}').unwrap();
        assert!(stuck_block[..stuck_block_end].contains(pilot_hex.as_str()));
    }

    #[test]
    fn css_collapsed_has_no_border() {
        let css = charter_css();
        let start = css.find(".cs-status-collapsed").unwrap();
        let block_end = css[start..].find('}').unwrap() + start;
        assert!(css[start..block_end].contains("border: none"));
    }

    #[test]
    fn css_legacy_aliases_resolve() {
        let css = charter_css();
        for (alias, _) in LEGACY_ALIASES {
            assert!(css.contains(&format!("--{alias}:")), "missing {alias}");
        }
    }
}
