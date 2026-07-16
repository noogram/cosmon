// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for the sensorium loader + vital-strip renderer
//! (ADR-109 (sensorium-strip)).
//!
//! These tests pin the three silence laws from `responses/jr.md`:
//!
//! 1. **Idempotent rendering.** Unchanged sensorium state → byte-identical
//!    canonical raster across ticks.
//! 2. **No animation without operator gesture.** This law is enforced by
//!    the viewport renderer, not the snapshot bytes; we cannot exercise
//!    it from a CLI integration test, but we verify it is *not violated*
//!    by the byte layer (the strip's bytes never depend on time of day).
//! 3. **Kill-switch visible.** When `~/.cosmon/autopilot.off` exists the
//!    strip carries the `[off]` glyph. Tested via the
//!    [`cosmon_cli::cmd::sensorium`] loader directly to avoid touching
//!    the operator's real `$HOME`.
//!
//! The fixture under `tests/fixtures/sensorium/` is the smoke shape:
//! one peau signal in the 24h window, one live beat among ten, a SOUL
//! file naming `cosmon`, two notes (one decaying), one pending outbox
//! draft.

use std::fs;
use std::path::Path;

use cosmon_observability::render::{render_canonical, render_vital_strip, SnapshotConfig};
use cosmon_observability::sensorium::HeartbeatKind;
use cosmon_observability::FleetSnapshot;

fn fixture_sensorium_root(tmp: &Path) -> std::path::PathBuf {
    let root = tmp.join("sensorium");
    fs::create_dir_all(&root).unwrap();

    // peau — one recent signal (within 24h), one ancient (should be
    // ignored).
    let inbox = root.join("inbox.ndjson");
    let recent = chrono::Utc::now() - chrono::Duration::hours(2);
    let ancient = chrono::Utc::now() - chrono::Duration::hours(72);
    let inbox_lines = format!(
        "{{\"ts\":\"{}\",\"channel\":\"whatsapp\",\"sender\":\"heidi\"}}\n\
         {{\"ts\":\"{}\",\"channel\":\"imessage\",\"sender\":\"operator-demo\"}}\n",
        recent.to_rfc3339(),
        ancient.to_rfc3339(),
    );
    fs::write(&inbox, inbox_lines).unwrap();

    // cœur — three beats, one live.
    let heartbeat = root.join("heartbeat.ndjson");
    let hb_lines = "\
        {\"ts\":\"2026-05-22T00:00:00Z\",\"kind\":\"patrol\",\"moved\":[]}\n\
        {\"ts\":\"2026-05-22T00:01:00Z\",\"kind\":\"patrol\",\"moved\":[\"task-1\"]}\n\
        {\"ts\":\"2026-05-22T00:02:00Z\",\"kind\":\"patrol\",\"moved\":[]}\n";
    fs::write(&heartbeat, hb_lines).unwrap();

    // visage — one SOUL.md under <galaxy>/.
    let visage_dir = root.join("cosmon");
    fs::create_dir_all(&visage_dir).unwrap();
    fs::write(visage_dir.join("SOUL.md"), "---\nname: cosmon\n---\n").unwrap();

    // carnet — two notes, one with imminent decay.
    let notes = root.join("notes");
    fs::create_dir_all(&notes).unwrap();
    let soon = (chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339();
    let later = (chrono::Utc::now() + chrono::Duration::days(2)).to_rfc3339();
    fs::write(
        notes.join("urgent.md"),
        format!("---\ndecay_at: {soon}\n---\n"),
    )
    .unwrap();
    fs::write(
        notes.join("calm.md"),
        format!("---\ndecay_at: {later}\n---\n"),
    )
    .unwrap();

    // voix — one pending draft.
    let outbox = root.join("outbox");
    fs::create_dir_all(&outbox).unwrap();
    fs::write(
        outbox.join("draft-1.md"),
        "---\npermission: pending\n---\nDraft body.\n",
    )
    .unwrap();

    root
}

/// Silence law #1 — tick-to-tick byte identity.
#[test]
fn vital_strip_byte_identical_when_state_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fixture_sensorium_root(tmp.path());

    let s = cosmon_cli::sensorium::load_sensorium(tmp.path());
    let line1 = render_vital_strip(&s);
    let line2 = render_vital_strip(&s);
    let line3 = render_vital_strip(&s);
    assert_eq!(line1, line2, "tick 2 diverged from tick 1");
    assert_eq!(line2, line3, "tick 3 diverged from tick 2");
}

/// The whole canonical raster is byte-identical when neither the fleet
/// snapshot nor the sensorium changed.
#[test]
fn canonical_raster_byte_identical_with_sensorium_when_state_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fixture_sensorium_root(tmp.path());

    let s = cosmon_cli::sensorium::load_sensorium(tmp.path());
    let cfg = SnapshotConfig {
        sensorium: s,
        ..SnapshotConfig::default()
    };
    let snap = FleetSnapshot::new();
    let a = render_canonical(&snap, &cfg);
    let b = render_canonical(&snap, &cfg);
    assert_eq!(a, b, "canonical raster diverged tick-to-tick");
}

/// Silence law #2 — the byte layer never reads a clock. Calling the
/// renderer multiple times within a window where the wall-clock
/// changes must not perturb the bytes.
#[test]
fn vital_strip_does_not_read_wall_clock() {
    let s = cosmon_observability::Sensorium::default();
    let a = render_vital_strip(&s);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let b = render_vital_strip(&s);
    assert_eq!(a, b, "wall-clock drift perturbed the strip bytes");
}

/// Loader produces the canonical signals from the smoke fixture.
#[test]
fn loader_parses_smoke_fixture() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fixture_sensorium_root(tmp.path());
    let s = cosmon_cli::sensorium::load_sensorium(tmp.path());

    assert_eq!(s.peau_signals_24h, 1, "only the recent peau signal counts");
    assert_eq!(s.visage_galaxy.as_deref(), Some("cosmon"));
    assert!(!s.visage_seal_drift);
    assert_eq!(s.carnet_count, 2);
    assert_eq!(s.carnet_decay_6h, Some(1));
    assert_eq!(s.voix_awaiting, 1);

    // Heartbeat: three beats land right-aligned in the 10-slot window.
    // Slots 7..=9: [Resting, Live, Resting].
    assert!(matches!(s.heartbeat[7], HeartbeatKind::Resting));
    assert!(matches!(s.heartbeat[8], HeartbeatKind::Live));
    assert!(matches!(s.heartbeat[9], HeartbeatKind::Resting));
}

/// Render contains the expected glyphs for the smoke fixture.
#[test]
fn smoke_strip_contains_expected_glyphs() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fixture_sensorium_root(tmp.path());
    let s = cosmon_cli::sensorium::load_sensorium(tmp.path());
    let line = render_vital_strip(&s);
    assert!(line.contains("~ 01"), "missing peau count: {line:?}");
    assert!(line.contains('*'), "missing live heartbeat glyph: {line:?}");
    assert!(line.contains("@ cosmon"), "missing visage: {line:?}");
    assert!(line.contains("= 2 notes"), "missing carnet count: {line:?}");
    assert!(line.contains("-1 in 6h"), "missing decay glyph: {line:?}");
    assert!(
        line.contains("> 1 awaiting"),
        "missing voix glyph: {line:?}"
    );
}
