// SPDX-License-Identifier: AGPL-3.0-only

//! `X` pane — eXceptions: recently collapsed molecules across the
//! current state dir, grouped by [`CollapseReason`] variant.
//!
//! Reads `events.jsonl` directly (no separate index) and renders a
//! short, scannable summary inline in the detail pane. Pairs with the
//! `cs errors` command — the pane is the in-TUI surface of the same
//! aggregation.

use std::collections::BTreeMap;
use std::io::BufRead as _;

use chrono::{DateTime, Utc};
use cosmon_core::event_v2::{CollapseReason, Envelope, EventV2};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::{DetailCtx, DetailRenderer};

pub(crate) struct ErrorsRenderer;

impl DetailRenderer for ErrorsRenderer {
    fn keys(&self) -> &'static [char] {
        // Capital `X` for eXceptions — 'e' is taken by events,
        // lowercase 'x' is reserved for future expansion.
        &['X']
    }

    fn label(&self) -> &'static str {
        "errors"
    }

    fn is_live(&self) -> bool {
        true
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let Some(sd) = ctx.state_dir else {
            return Text::raw("<no state dir>");
        };
        let path = sd.join("events.jsonl");
        let Ok(file) = std::fs::File::open(&path) else {
            return Text::raw("<no events.jsonl>");
        };

        let mut records: Vec<CollapseRecord> = std::io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| Envelope::from_line(&l).ok())
            .filter_map(extract_record)
            .collect();

        // Most recent first.
        records.sort_by_key(|x| std::cmp::Reverse(x.timestamp));

        if records.is_empty() {
            return Text::raw("<no collapses recorded>");
        }

        let total = records.len();
        let structured = records.iter().filter(|r| r.structured).count();
        let pct = if total == 0 {
            0.0
        } else {
            (structured as f64 / total as f64) * 100.0
        };

        let mut by_kind: BTreeMap<String, u32> = BTreeMap::new();
        for r in &records {
            *by_kind.entry(r.kind.clone()).or_insert(0) += 1;
        }

        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().fg(Color::DarkGray);
        let hot = Style::default().fg(Color::Red);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                "eXceptions  ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("total={total}  "), bold),
            Span::raw(format!("structured={structured} ({pct:.1}%)  ")),
            Span::raw(format!("other={}", total - structured)),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "by kind (descending)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        let mut kinds: Vec<(String, u32)> = by_kind.into_iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (kind, count) in kinds.iter().take(8) {
            let kind_disp = if kind.chars().count() > 28 {
                let mut s: String = kind.chars().take(27).collect();
                s.push('…');
                s
            } else {
                kind.clone()
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{kind_disp:<28}"), hot),
                Span::raw(format!(" ×{count}")),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "recent collapses (most recent first, max 50)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for r in records.iter().take(50) {
            let ts_disp = r.timestamp.format("%Y-%m-%d %H:%M:%SZ").to_string();
            let kind_disp = if r.kind.chars().count() > 22 {
                let mut s: String = r.kind.chars().take(21).collect();
                s.push('…');
                s
            } else {
                r.kind.clone()
            };
            let reason_disp = if r.reason.chars().count() > 60 {
                let mut s: String = r.reason.chars().take(59).collect();
                s.push('…');
                s
            } else {
                r.reason.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(ts_disp, dim),
                Span::raw("  "),
                Span::styled(format!("{kind_disp:<22}"), hot),
                Span::raw("  "),
                Span::styled(r.molecule_id.clone(), bold),
                Span::raw("  "),
                Span::styled(reason_disp, dim),
            ]));
        }

        Text::from(lines)
    }
}

struct CollapseRecord {
    molecule_id: String,
    kind: String,
    structured: bool,
    reason: String,
    timestamp: DateTime<Utc>,
}

fn extract_record(env: Envelope) -> Option<CollapseRecord> {
    let EventV2::MoleculeCollapsed {
        molecule_id,
        reason,
        kind,
    } = env.event
    else {
        return None;
    };
    let resolved = kind
        .clone()
        .unwrap_or_else(|| CollapseReason::Other(reason.clone()));
    Some(CollapseRecord {
        molecule_id: molecule_id.as_str().to_owned(),
        kind: resolved.as_str().to_owned(),
        structured: resolved.is_structured(),
        reason,
        timestamp: env.timestamp,
    })
}
