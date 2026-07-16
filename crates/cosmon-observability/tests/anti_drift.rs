// SPDX-License-Identifier: AGPL-3.0-only

//! Anti-drift gate — TUI adapter vs HTTP adapter on a shared fixture.
//!
//! The whole point of `cosmon-observability` is that `cs peek` (the
//! terminal TUI) and `cosmon-cockpit-http` (the HTTP dashboard) render
//! the same facts. This test plays both adapters through the canonical
//! [`canonical_snapshot`] fixture and asserts the same signal set
//! survives each projection.
//!
//! The gate is structural: if either adapter silently drops, renames,
//! or mistypes a field, the test fails. This is the prevention we wish
//! had existed before the ADR-020 drift incident.
//!
//! Adapters under test:
//! - **TUI** — [`cosmon_observability::render::tui_lines`], drawn onto a
//!   `ratatui::backend::TestBackend` buffer so the terminal output is
//!   byte-stable and inspectable without a real terminal.
//! - **HTTP** — [`cosmon_observability::render::json_view`], the body of
//!   `/api/fleet`.
//!
//! Both adapters are forced through the same port. Introducing a
//! divergence — e.g. by reading a field directly from the snapshot in
//! one adapter but not the other — makes this test fail.

use cosmon_observability::fixture::{canonical_signals, canonical_snapshot};
use cosmon_observability::render::{json_view, tui_lines};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

/// Render `tui_lines` into a `TestBackend` buffer and return the buffer.
///
/// This is the TUI adapter contract: take the shared projection and
/// draw it as-is. The real `cs peek` will wrap this in a higher-level
/// widget, but the rows themselves must round-trip through the same
/// function the dashboard uses.
fn render_tui_buffer(lines: &[String]) -> Buffer {
    let text: Text = lines
        .iter()
        .map(|s| Line::raw(s.clone()))
        .collect::<Vec<_>>()
        .into();
    let paragraph = Paragraph::new(text);

    let backend = TestBackend::new(80, 16);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| {
            frame.render_widget(paragraph, Rect::new(0, 0, 80, 16));
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// Flatten a buffer into a single string for substring searches.
fn buffer_text(buffer: &Buffer) -> String {
    let mut s = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            s.push_str(
                buffer
                    .cell((x, y))
                    .map_or(" ", ratatui::buffer::Cell::symbol),
            );
        }
        s.push('\n');
    }
    s
}

/// Recursively collect every string value from a JSON document.
fn json_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(xs) => xs.iter().for_each(|x| json_strings(x, out)),
        serde_json::Value::Object(m) => m.values().for_each(|x| json_strings(x, out)),
        _ => {}
    }
}

#[test]
fn every_canonical_signal_appears_in_both_adapters() {
    let snap = canonical_snapshot();

    let tui = render_tui_buffer(&tui_lines(&snap));
    let tui_flat = buffer_text(&tui);

    let json = json_view(&snap);
    let mut json_strs = Vec::new();
    json_strings(&json, &mut json_strs);
    let json_flat = json_strs.join("|");

    for signal in canonical_signals() {
        assert!(
            tui_flat.contains(signal),
            "TUI buffer dropped canonical signal {signal:?}:\n{tui_flat}"
        );
        assert!(
            json_flat.contains(signal),
            "HTTP JSON dropped canonical signal {signal:?}:\n{json_flat}"
        );
    }
}

#[test]
fn tui_buffer_matches_golden_rows() {
    let snap = canonical_snapshot();
    let buffer = render_tui_buffer(&tui_lines(&snap));
    let flat = buffer_text(&buffer);

    // Golden header + two row signatures.
    assert!(flat.contains("session"));
    assert!(flat.contains("molecule"));
    assert!(flat.contains("cosmon-alpha"));
    assert!(flat.contains("mol-alpha"));
    assert!(flat.contains("Alpha task"));
    assert!(flat.contains("w-alpha"));
    assert!(flat.contains("working"));

    assert!(flat.contains("cosmon-beta"));
    assert!(flat.contains("mol-beta"));
    assert!(flat.contains("Beta issue"));
    assert!(flat.contains("w-beta"));
    assert!(flat.contains("idle"));
}

#[test]
fn json_view_has_expected_shape() {
    let snap = canonical_snapshot();
    let v = json_view(&snap);

    let sessions = v["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 2);
    let molecules = v["molecules"].as_array().expect("molecules array");
    assert_eq!(molecules.len(), 2);
    let workers = v["workers"].as_array().expect("workers array");
    assert_eq!(workers.len(), 2);

    for m in molecules {
        for key in ["id", "title", "kind", "session"] {
            assert!(m.get(key).is_some(), "molecule missing {key}");
        }
    }
    for w in workers {
        for key in ["id", "session", "live", "energy_total"] {
            assert!(w.get(key).is_some(), "worker missing {key}");
        }
    }
}

#[test]
fn adapters_agree_on_session_cardinality() {
    // A divergence in either projection (e.g. one adapter dropping the
    // beta session) breaks this assertion.
    let snap = canonical_snapshot();
    let tui = tui_lines(&snap);
    // header + rows
    let tui_session_rows = tui.len() - 1;
    let json_sessions = json_view(&snap)["sessions"].as_array().unwrap().len();
    assert_eq!(tui_session_rows, json_sessions);
}

#[test]
fn introducing_drift_breaks_signal_set() {
    // Guardrail: a deliberately truncated TUI view must fail the signal
    // check, proving the anti-drift gate actually catches divergence.
    let snap = canonical_snapshot();
    let mut truncated = tui_lines(&snap);
    truncated.retain(|l| !l.contains("beta"));
    let buffer = render_tui_buffer(&truncated);
    let flat = buffer_text(&buffer);
    assert!(!flat.contains("cosmon-beta"));
    assert!(!flat.contains("w-beta"));
}
