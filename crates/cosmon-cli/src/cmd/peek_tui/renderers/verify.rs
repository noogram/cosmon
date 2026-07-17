// SPDX-License-Identifier: AGPL-3.0-only

//! Renders the claim-level breakdown of `verify-report.md` — opened via
//! the `v` key. One line per claim with a verdict glyph, category, name,
//! and truncated detail; a summary footer shows PASS/FAIL/SKIP counts and
//! the lineage-coverage percentage. Falls back to `<not verified>` when
//! the report is missing.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::super::trust::{self, ClaimStatus};
use super::{DetailCtx, DetailRenderer};

pub(crate) struct VerifyRenderer;

impl DetailRenderer for VerifyRenderer {
    fn keys(&self) -> &'static [char] {
        &['v']
    }

    fn label(&self) -> &'static str {
        "verify"
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let Some(dir) = ctx.molecule_dir else {
            return Text::raw("<no molecule directory>");
        };
        let Some(report) = trust::load_report(dir) else {
            return Text::from(vec![
                Line::from(Span::styled(
                    "<not verified>",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::raw(format!(
                    "Run `cs verify {}` to seal the proof-of-work chain.",
                    ctx.row.mol_id
                ))),
            ]);
        };

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(report.claims.len() + 4);
        lines.push(Line::from(Span::styled(
            "Claim-level verification",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        for claim in &report.claims {
            let (glyph, glyph_style) = match claim.status {
                ClaimStatus::Pass => ("✓", Style::default().fg(Color::Green)),
                ClaimStatus::Fail => ("✗", Style::default().fg(Color::Red)),
                ClaimStatus::Skip => ("·", Style::default().fg(Color::DarkGray)),
            };
            let detail = truncate(&claim.detail, 60);
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} "), glyph_style),
                Span::styled(
                    format!("[{}] ", claim.category),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(claim.name.clone()),
                Span::styled(
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(" — {detail}")
                    },
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(summary_line(&report));
        Text::from(lines)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn summary_line(report: &trust::TrustReport) -> Line<'static> {
    let pass = report.pass();
    let fail = report.fail();
    let skip = report.skip();
    let coverage = report
        .coverage_pct()
        .map_or_else(|| "—".to_owned(), |p| format!("{p}%"));
    Line::from(vec![
        Span::styled("Summary: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{pass} PASS"), Style::default().fg(Color::Green)),
        Span::raw(" · "),
        Span::styled(format!("{fail} FAIL"), Style::default().fg(Color::Red)),
        Span::raw(" · "),
        Span::styled(format!("{skip} SKIP"), Style::default().fg(Color::DarkGray)),
        Span::raw("   coverage "),
        Span::styled(coverage, Style::default().add_modifier(Modifier::BOLD)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::peek_tui::RowView;
    use cosmon_observability::HeartbeatTier;

    fn test_row() -> RowView {
        RowView {
            mol_id: "task-aaaa".into(),
            session_slug: None,
            project: "p".into(),
            role: "worker".into(),
            status: "completed".into(),
            step: "1/1".into(),
            updated_at: None,
            energy_in: 0,
            energy_out: 0,
            cost_usd: 0.0,
            context_window: None,
            session: None,
            socket: "s".into(),
            heartbeat: HeartbeatTier::Active,
            last_activity: None,
            last_progress_at: None,
            topic: None,
            mission_description: None,
            formula: String::new(),
            tier_badge: String::new(),
            kind: String::new(),
            blocked_by: Vec::new(),
            worker_name: None,
            tags: Vec::new(),
            created_at_utc: None,
            whisper_fresh: false,
            role_glyphs: String::new(),
            trust_score: None,
            energy_budget: None,
            adapter: cosmon_core::adapter_attribution::AdapterAttribution::default(),
        }
    }

    #[test]
    fn missing_report_renders_hint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let row = test_row();
        let ctx = DetailCtx {
            row: &row,
            molecule_dir: Some(tmp.path()),
            state_dir: None,
        };
        let out = VerifyRenderer.render(&ctx);
        let text = flatten(&out);
        assert!(text.contains("not verified"));
        assert!(text.contains("cs verify"));
    }

    #[test]
    fn present_report_renders_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("verify-report.md"),
            "\
| Status | Category | Name | Detail |
|--------|----------|------|--------|
| PASS | artifact | `a.md` | blake3 ok |
| FAIL | gate | `t` | exit 1 |
",
        )
        .unwrap();
        let row = test_row();
        let ctx = DetailCtx {
            row: &row,
            molecule_dir: Some(tmp.path()),
            state_dir: None,
        };
        let out = VerifyRenderer.render(&ctx);
        let text = flatten(&out);
        assert!(text.contains("1 PASS"));
        assert!(text.contains("1 FAIL"));
        assert!(text.contains("50%"));
    }

    fn flatten(t: &Text<'_>) -> String {
        t.lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
