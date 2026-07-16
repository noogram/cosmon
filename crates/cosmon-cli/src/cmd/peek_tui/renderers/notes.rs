// SPDX-License-Identifier: AGPL-3.0-only

//! Renders every file under `notes/` — freeform worker notes.

use ratatui::text::Text;

use super::{render_dir, DetailCtx, DetailRenderer};

pub(crate) struct NotesRenderer;

impl DetailRenderer for NotesRenderer {
    fn keys(&self) -> &'static [char] {
        // `n` is reserved for the nucleate action keystroke (delib-20260423-becf,
        // task-20260423-16ad). The notes detail pane moves to the shifted
        // letter so `n` can launch the nucleate modal.
        &['N']
    }

    fn label(&self) -> &'static str {
        "notes"
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        Text::raw(render_dir(ctx.molecule_dir, "notes", "<no notes yet>"))
    }
}
