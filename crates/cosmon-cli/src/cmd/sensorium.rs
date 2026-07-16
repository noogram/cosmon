// SPDX-License-Identifier: AGPL-3.0-only

//! `cs sensorium` — five-organ vital-strip reader (CLI surface).
//!
//! Thin command wrapper around [`cosmon_cli::sensorium::load_sensorium`]
//! (the loader lives in the crate's `lib.rs` so external integration
//! tests can compute the strip without shelling out to this binary).
//! Emits the aggregate as a one-line JSON object (`--json`) or a
//! human-readable ASCII summary keyed by glyph.
//!
//! UX↔CLI parity (ADR-068): every byte in `cs peek --snapshot`'s
//! vital strip is queryable here without re-parsing ASCII — viewports
//! that want to re-render in their native vocabulary read the JSON
//! shape instead.

use std::io::Write;

use cosmon_observability::sensorium::{HeartbeatKind, HEARTBEAT_WINDOW};

use super::Context;
use crate::sensorium::load_sensorium;

/// Arguments for `cs sensorium`.
#[derive(clap::Args)]
pub struct Args {
    /// Emit the aggregate as a single JSON line instead of an ASCII
    /// summary. The global `--json` flag also enables this; the
    /// explicit flag is kept for discoverability and ADR-068 UX↔CLI
    /// parity.
    #[arg(long)]
    pub json: bool,
}

/// Execute `cs sensorium`.
///
/// # Errors
///
/// Returns the underlying I/O error if `stdout` cannot be written.
/// File-read failures inside the sensorium tree are intentionally
/// swallowed — the silence rule applies (see `crate::sensorium` docs).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let s = load_sensorium(&state_dir);

    let want_json = ctx.json || args.json;
    let mut out = std::io::stdout().lock();
    if want_json {
        let line = serde_json::to_string(&s.to_json())?;
        writeln!(out, "{line}")?;
    } else {
        writeln!(out, "~ {:02}  signals (24h)", s.peau_signals_24h.min(99))?;
        writeln!(out, "{}  heartbeat", heartbeat_line(&s.heartbeat))?;
        let galaxy = s.visage_galaxy.as_deref().unwrap_or("<galaxy>");
        let suffix = if s.visage_seal_drift { "!" } else { "" };
        writeln!(out, "@ {galaxy}{suffix}  visage")?;
        let decay = s
            .carnet_decay_6h
            .map_or_else(String::new, |d| format!(" (-{d} in 6h)"));
        writeln!(out, "= {} notes{decay}  carnet", s.carnet_count)?;
        writeln!(out, "> {} awaiting  voix", s.voix_awaiting.min(9))?;
        if s.autopilot_off {
            writeln!(out, "[off]  kill-switch")?;
        }
    }
    Ok(())
}

fn heartbeat_line(beats: &[HeartbeatKind; HEARTBEAT_WINDOW]) -> String {
    let mut s = String::with_capacity(2 * HEARTBEAT_WINDOW);
    for (i, b) in beats.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push(b.glyph());
    }
    s
}
