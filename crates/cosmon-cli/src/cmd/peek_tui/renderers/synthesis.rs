// SPDX-License-Identifier: AGPL-3.0-only

//! Renders `synthesis.md` — the converged output of a deliberation
//! molecule. Empty / missing on every other kind of molecule. Pretty-
//! printed via `tui-markdown`.

use ratatui::text::Text;

use super::{read_artifact_file, render_markdown, DetailCtx, DetailRenderer};

pub(crate) struct SynthesisRenderer;

impl DetailRenderer for SynthesisRenderer {
    fn keys(&self) -> &'static [char] {
        &['s']
    }

    fn label(&self) -> &'static str {
        "synthesis"
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let raw = read_artifact_file(ctx.molecule_dir, "synthesis.md", "<not a deliberation>");
        render_markdown(&raw)
    }
}
