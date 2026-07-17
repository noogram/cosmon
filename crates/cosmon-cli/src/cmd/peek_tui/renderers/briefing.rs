// SPDX-License-Identifier: AGPL-3.0-only

//! Renders the molecule's mission context followed by `briefing.md` — the
//! formula-generated prompt the worker received on spawn. The mission comes
//! first because it explains the molecule-specific purpose before the shared
//! formula template. Both are pretty-printed via `tui-markdown` so headings,
//! code fences, and checklists render as styled text instead of raw Markdown
//! source.

use ratatui::text::Text;

use super::{read_artifact_file, render_markdown, DetailCtx, DetailRenderer};

pub(crate) struct BriefingRenderer;

impl DetailRenderer for BriefingRenderer {
    fn keys(&self) -> &'static [char] {
        &['b']
    }

    fn label(&self) -> &'static str {
        "briefing"
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let raw = read_artifact_file(ctx.molecule_dir, "briefing.md", "<no briefing.md>");
        let mut document = mission_markdown(ctx);
        document.push_str(&raw);
        render_markdown(&document)
    }
}

/// Render only the operator-supplied mission fields, leaving formula content
/// in the sealed briefing artifact below. Missing fields are omitted so
/// legacy molecules remain readable without invented mission data.
fn mission_markdown(ctx: &DetailCtx<'_>) -> String {
    let topic = ctx.row.topic.as_deref();
    let description = ctx.row.mission_description.as_deref();
    if topic.is_none() && description.is_none() {
        return String::new();
    }

    let mut mission = String::from("# Mission\n\n");
    if let Some(topic) = topic {
        mission.push_str("## Topic\n\n");
        mission.push_str(topic);
        mission.push_str("\n\n");
    }
    if let Some(description) = description {
        mission.push_str("## Description\n\n");
        mission.push_str(description);
        mission.push_str("\n\n");
    }
    mission.push_str("---\n\n");
    mission
}

#[cfg(test)]
mod tests {
    use super::mission_markdown;
    use crate::cmd::peek_tui::renderers::DetailCtx;
    use crate::cmd::peek_tui::RowView;

    fn row(topic: Option<&str>, description: Option<&str>) -> RowView {
        RowView {
            mol_id: "task-1".into(),
            session_slug: None,
            project: String::new(),
            role: String::new(),
            status: String::new(),
            step: String::new(),
            updated_at: None,
            energy_in: 0,
            energy_out: 0,
            cost_usd: 0.0,
            context_window: None,
            session: None,
            socket: String::new(),
            heartbeat: cosmon_observability::HeartbeatTier::Active,
            last_activity: None,
            last_progress_at: None,
            topic: topic.map(str::to_owned),
            mission_description: description.map(str::to_owned),
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
    fn mission_precedes_the_formula_briefing_with_operator_context() {
        let row = row(
            Some("Repair peek briefing"),
            Some("Show the molecule mission."),
        );
        let ctx = DetailCtx {
            row: &row,
            molecule_dir: None,
            state_dir: None,
        };

        assert_eq!(
            mission_markdown(&ctx),
            "# Mission\n\n## Topic\n\nRepair peek briefing\n\n## Description\n\n\
             Show the molecule mission.\n\n---\n\n"
        );
    }

    #[test]
    fn mission_omits_an_invented_header_for_legacy_molecules() {
        let row = row(None, None);
        let ctx = DetailCtx {
            row: &row,
            molecule_dir: None,
            state_dir: None,
        };

        assert!(mission_markdown(&ctx).is_empty());
    }
}
