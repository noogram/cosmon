// SPDX-License-Identifier: AGPL-3.0-only

//! Renders the molecule's `briefing.md` — the formula-generated prompt
//! the worker received on spawn. Pretty-printed via `tui-markdown` so
//! headings, code fences, and checklists render as styled text instead
//! of raw Markdown source.

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
        render_markdown(&raw)
    }
}
