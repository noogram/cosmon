// SPDX-License-Identifier: AGPL-3.0-only

//! Renders the tail of `events.jsonl` filtered to events mentioning the
//! selected molecule id. Each line is parsed as JSON and pretty-printed
//! as `HH:MM:SS  KIND  summary` with color by kind — far more scannable
//! than the raw JSONL.

use std::io::BufRead as _;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::{DetailCtx, DetailRenderer};

pub(crate) struct EventsRenderer;

impl DetailRenderer for EventsRenderer {
    fn keys(&self) -> &'static [char] {
        &['e']
    }

    fn label(&self) -> &'static str {
        "events"
    }

    fn is_live(&self) -> bool {
        true
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let Some(sd) = ctx.state_dir else {
            return Text::raw("<no state dir>");
        };
        let mol_id = &ctx.row.mol_id;
        let path = sd.join("events.jsonl");
        let Ok(file) = std::fs::File::open(&path) else {
            return Text::raw("<no events.jsonl>");
        };
        let mut matched: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter(|l| l.contains(mol_id.as_str()))
            .collect();
        if matched.len() > 200 {
            let drop = matched.len() - 200;
            matched.drain(0..drop);
        }
        if matched.is_empty() {
            return Text::raw("<no events for this molecule>");
        }
        let lines: Vec<Line<'static>> = matched.iter().map(|l| format_event_line(l)).collect();
        Text::from(lines)
    }
}

/// Format a single JSONL event into a styled line. Falls back to the raw
/// line when the JSON is malformed — we never drop content.
fn format_event_line(raw: &str) -> Line<'static> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Line::raw(raw.to_string());
    };
    let ts = v
        .get("timestamp")
        .or_else(|| v.get("ts"))
        .and_then(|t| t.as_str())
        .map(short_time)
        .unwrap_or_else(|| "--:--:--".into());
    let kind = v
        .get("kind")
        .or_else(|| v.get("event"))
        .or_else(|| v.get("type"))
        .and_then(|k| k.as_str())
        .unwrap_or("event")
        .to_string();
    let summary = summarize(&v);

    Line::from(vec![
        Span::styled(ts, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(
            format!("{kind:<18}"),
            Style::default()
                .fg(kind_color(&kind))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(summary),
    ])
}

/// Keep only `HH:MM:SS` from an ISO-8601 / RFC-3339 timestamp. Accepts
/// anything — if we can't find a `T`, we just truncate.
fn short_time(ts: &str) -> String {
    if let Some((_, rest)) = ts.split_once('T') {
        rest.chars().take(8).collect()
    } else {
        ts.chars().take(8).collect()
    }
}

fn kind_color(kind: &str) -> Color {
    match kind {
        "nucleated" | "tackled" | "dispatched" => Color::Cyan,
        "evolved" | "step_completed" => Color::Blue,
        "completed" | "done" => Color::Green,
        "stuck" | "collapsed" | "failed" | "error" => Color::Red,
        "freeze" | "thaw" => Color::Magenta,
        "heartbeat" => Color::DarkGray,
        _ => Color::Yellow,
    }
}

/// Build a one-line summary from the remaining JSON fields. Pulls a few
/// common keys (`step`, `reason`, `evidence`, `summary`, `message`) and
/// ignores noisy / structural ones.
fn summarize(v: &serde_json::Value) -> String {
    const SKIP: &[&str] = &[
        "timestamp",
        "ts",
        "kind",
        "event",
        "type",
        "molecule_id",
        "mol_id",
        "fleet_id",
        "agent_id",
    ];
    const PREFERRED: &[&str] = &["step", "reason", "evidence", "summary", "message", "detail"];

    let Some(obj) = v.as_object() else {
        return String::new();
    };

    let mut parts: Vec<String> = Vec::new();
    for k in PREFERRED {
        if let Some(val) = obj.get(*k) {
            parts.push(format!("{k}={}", scalar(val)));
        }
    }
    if parts.is_empty() {
        for (k, val) in obj {
            if SKIP.contains(&k.as_str()) {
                continue;
            }
            parts.push(format!("{k}={}", scalar(val)));
            if parts.len() >= 4 {
                break;
            }
        }
    }
    parts.join("  ")
}

fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => {
            let one = s.replace('\n', " ⏎ ");
            if one.chars().count() > 80 {
                let trimmed: String = one.chars().take(80).collect();
                format!("{trimmed}…")
            } else {
                one
            }
        }
        serde_json::Value::Null => "null".into(),
        other => other.to_string(),
    }
}
