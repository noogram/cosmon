// SPDX-License-Identifier: AGPL-3.0-only

//! Renders every file under `responses/` — used by deliberation
//! molecules to collect per-persona responses.

use ratatui::text::Text;

use super::{render_dir, DetailCtx, DetailRenderer};

pub(crate) struct ResponsesRenderer;

impl DetailRenderer for ResponsesRenderer {
    fn keys(&self) -> &'static [char] {
        &['r']
    }

    fn label(&self) -> &'static str {
        "responses"
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        Text::raw(render_dir(ctx.molecule_dir, "responses", "<no responses/>"))
    }
}
