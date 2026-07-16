// SPDX-License-Identifier: AGPL-3.0-only

//! Detail-pane rendering plugged into `cs peek` via the [`DetailRenderer`]
//! trait. Each pane (briefing, log, events, git, …) is a small unit struct
//! implementing the trait in its own module, and [`App`](super::App) holds a
//! `Vec<Box<dyn DetailRenderer>>` instead of a giant `match DetailPane`.
//!
//! The trait is intentionally `pub(crate)` — there is no external consumer
//! today, and freezing [`DetailCtx`]'s shape would be premature.

use std::path::Path;

use ratatui::text::Text;

use super::RowView;

mod briefing;
mod errors;
mod events;
mod git;
mod log;
mod notes;
mod responses;
mod synthesis;
mod tmux;
mod tree;
mod verify;

pub(crate) use briefing::BriefingRenderer;
pub(crate) use errors::ErrorsRenderer;
pub(crate) use events::EventsRenderer;
pub(crate) use git::GitRenderer;
pub(crate) use log::LogRenderer;
pub(crate) use notes::NotesRenderer;
pub(crate) use responses::ResponsesRenderer;
pub(crate) use synthesis::SynthesisRenderer;
pub(crate) use tmux::TmuxRenderer;
pub(crate) use tree::TreeRenderer;
pub(crate) use verify::VerifyRenderer;

/// Everything a detail renderer needs to produce its pane body. Carries
/// references only — cheap to construct per keypress.
pub(crate) struct DetailCtx<'a> {
    /// The currently-selected row. Renderers inspect its fields to decide
    /// what artifact to read.
    pub(crate) row: &'a RowView,
    /// Directory holding per-molecule artifacts (`briefing.md`, `log.md`,
    /// `synthesis.md`, `notes/`, `responses/`, …). `None` when the molecule
    /// has no resolvable state dir.
    pub(crate) molecule_dir: Option<&'a Path>,
    /// Directory of the owning fleet's state store — siblings include
    /// `events.jsonl` and (two levels up) the project root used by
    /// [`GitRenderer`] to locate the worktree.
    pub(crate) state_dir: Option<&'a Path>,
}

/// A single pane in the detail side of `cs peek`. One struct per pane,
/// owned by [`super::App`] as `Box<dyn DetailRenderer>`. Adding a new pane
/// is a single new module + one line in [`all()`].
pub(crate) trait DetailRenderer {
    /// Keyboard keys that toggle this pane. The first key is considered
    /// canonical. Multiple keys are supported so legacy bindings (e.g.
    /// `p` + `Space` both map to [`TmuxRenderer`]) keep working.
    fn keys(&self) -> &'static [char];

    /// Short label shown in the pane title bar.
    fn label(&self) -> &'static str;

    /// `true` when the pane's content changes on its own (tmux capture,
    /// log tail, events, git). Live panes are re-rendered every refresh
    /// tick; static ones only re-fetch on selection / pane change.
    fn is_live(&self) -> bool {
        false
    }

    /// Produce the styled text shown in the right-hand pane. Renderers
    /// that want plain output can return `Text::raw(s)`; those wanting
    /// ANSI pass-through, markdown, or hand-rolled highlighting can
    /// build styled [`Line`](ratatui::text::Line)s directly.
    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static>;
}

/// The full registry of panes wired into `cs peek`. Order determines the
/// footer-hint ordering; key-to-renderer lookup is by scan.
pub(crate) fn all() -> Vec<Box<dyn DetailRenderer>> {
    vec![
        Box::new(TmuxRenderer),
        Box::new(BriefingRenderer),
        Box::new(LogRenderer),
        Box::new(EventsRenderer),
        Box::new(SynthesisRenderer),
        Box::new(ResponsesRenderer),
        Box::new(NotesRenderer),
        Box::new(GitRenderer),
        Box::new(TreeRenderer),
        Box::new(VerifyRenderer),
        Box::new(ErrorsRenderer),
    ]
}

// ---- shared helpers ----------------------------------------------------

/// Read a single artifact file from the molecule directory, falling back
/// to `fallback` on missing / empty file or when the directory is not
/// resolvable.
pub(super) fn read_artifact_file(
    molecule_dir: Option<&Path>,
    name: &str,
    fallback: &str,
) -> String {
    let Some(dir) = molecule_dir else {
        return fallback.into();
    };
    match std::fs::read_to_string(dir.join(name)) {
        Ok(s) if !s.is_empty() => s,
        _ => fallback.into(),
    }
}

/// Pretty-print a markdown document for the detail pane. Uses
/// `tui-markdown` for headings / emphasis / code fences, then deep-clones
/// the result into an owned [`Text`] (the crate returns borrowed spans
/// keyed to the input string, which won't outlive this call).
pub(super) fn render_markdown(src: &str) -> Text<'static> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    // `tui-markdown` is built against `ratatui-core` — a separate crate
    // from our `ratatui` dependency — so its `Style` / `Color` / `Modifier`
    // types don't unify with ours. We reconstruct equivalents field-by-
    // field, which also pins the mapping in one place if the upstream
    // types drift.
    fn conv_color(c: ratatui_core_style::Color) -> Color {
        use ratatui_core_style::Color as C;
        match c {
            C::Reset => Color::Reset,
            C::Black => Color::Black,
            C::Red => Color::Red,
            C::Green => Color::Green,
            C::Yellow => Color::Yellow,
            C::Blue => Color::Blue,
            C::Magenta => Color::Magenta,
            C::Cyan => Color::Cyan,
            C::Gray => Color::Gray,
            C::DarkGray => Color::DarkGray,
            C::LightRed => Color::LightRed,
            C::LightGreen => Color::LightGreen,
            C::LightYellow => Color::LightYellow,
            C::LightBlue => Color::LightBlue,
            C::LightMagenta => Color::LightMagenta,
            C::LightCyan => Color::LightCyan,
            C::White => Color::White,
            C::Rgb(r, g, b) => Color::Rgb(r, g, b),
            C::Indexed(i) => Color::Indexed(i),
        }
    }
    fn conv_modifier(m: ratatui_core_style::Modifier) -> Modifier {
        // Both types are `bitflags!` over a `u16` with the same variant
        // names; safest is bit-by-bit.
        Modifier::from_bits_truncate(m.bits())
    }
    fn conv_style(s: ratatui_core_style::Style) -> Style {
        let mut out = Style::default();
        if let Some(fg) = s.fg {
            out = out.fg(conv_color(fg));
        }
        if let Some(bg) = s.bg {
            out = out.bg(conv_color(bg));
        }
        out.add_modifier = conv_modifier(s.add_modifier);
        out.sub_modifier = conv_modifier(s.sub_modifier);
        out
    }

    let rendered = tui_markdown::from_str(src);
    let owned_lines: Vec<Line<'static>> = rendered
        .lines
        .into_iter()
        .map(|l| {
            let spans: Vec<Span<'static>> = l
                .spans
                .into_iter()
                .map(|s| Span::styled(s.content.into_owned(), conv_style(s.style)))
                .collect();
            Line::from(spans)
        })
        .collect();
    Text::from(owned_lines)
}

// Re-import `ratatui-core` under a unique alias so we can name its types
// in the conversion functions above. `tui-markdown` re-exports the crate
// at the same version we pull in transitively.
use ratatui_core::style as ratatui_core_style;

/// Concatenate every regular file under `molecule_dir/subdir`, separated
/// by a `── filename ──` header. Used for `responses/` and `notes/` —
/// two panes whose content shape is "a directory of markdown files".
pub(super) fn render_dir(molecule_dir: Option<&Path>, subdir: &str, empty: &str) -> String {
    use std::fmt::Write as _;

    let Some(dir) = molecule_dir else {
        return empty.into();
    };
    let target = dir.join(subdir);
    let Ok(read) = std::fs::read_dir(&target) else {
        return empty.into();
    };
    let mut entries: Vec<_> = read
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .collect();
    if entries.is_empty() {
        return empty.into();
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut out = String::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let _ = writeln!(out, "── {name} ──");
        match std::fs::read_to_string(e.path()) {
            Ok(body) => {
                out.push_str(&body);
                if !body.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(err) => {
                let _ = writeln!(out, "<read error: {err}>");
            }
        }
        out.push('\n');
    }
    out
}
