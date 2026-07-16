// SPDX-License-Identifier: AGPL-3.0-only

//! Retrospective adapter/model attribution — the honest projection of which
//! adapter (and, when pinned, which model) *actually* ran for a molecule,
//! folded from the durable `events.jsonl` record.
//!
//! # Why this lives in the zero-I/O core
//!
//! Both `cs peek` (TUI) and `cosmon-cockpit-http` (HTTP dashboard) need to
//! answer the same question — "which adapter did this molecule dispatch to,
//! and where did the choice come from?" If each surface folded the event log
//! its own way they would drift. Keeping the fold *and* the compact renderer
//! here, in the pure core, makes this the single source of truth the two
//! surfaces render through. Reading the log from disk is the caller's job
//! (the shell); this module only folds an already-read slice of events.
//!
//! # The honesty rule (never infer thinking from current config)
//!
//! [`AdapterAttribution::reasoning_effort`] is surfaced **only** when a past
//! event honestly recorded it. Cosmon does not persist reasoning/thinking
//! effort on any spawn-time event today ([`crate::event_v2::AdapterSelected`]
//! and [`crate::event_v2::ModelSelected`] carry no effort field), so a fold
//! always yields `None` for effort. It is **never** back-filled from the
//! current `.cosmon/config.toml` or a live `ModelSpec` — attributing today's
//! setting to yesterday's run would be a lie the operator cannot detect. The
//! field exists so that *if* a future event ever records the effort, the
//! whole pipeline surfaces it with no further change; until then, silence.

use crate::event_v2::{AdapterSelectionSource, EventV2, ModelSelectionSource};
use crate::model_spec::ReasoningEffort;

/// The honest, retrospective attribution of a molecule's dispatch, folded
/// from its `events.jsonl` slice.
///
/// Every field is `Option` because the record may be absent: a legacy
/// molecule predating [`EventV2::AdapterSelected`], a pending molecule never
/// tackled, or the safe model floor (`None` means "the adapter's own default
/// applied", never a fabricated id). A fully-empty attribution renders as
/// [`EMPTY_CELL`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterAttribution {
    /// The adapter name actually selected (e.g. `"claude"`), from the most
    /// recent [`EventV2::AdapterSelected`]. `None` when no selection was ever
    /// recorded for this molecule.
    pub adapter: Option<String>,
    /// Where the adapter choice came from, as a compact tag
    /// ([`AdapterSource`]). Paired with [`Self::adapter`].
    pub adapter_source: Option<AdapterSource>,
    /// The model id pinned for this dispatch (e.g. `"claude-opus-4-8"`), from
    /// the most recent [`EventV2::ModelSelected`]. `None` when nothing pinned
    /// a model and the adapter's own default applied — the safe floor, never
    /// a fabricated strong id.
    pub model: Option<String>,
    /// Where the model choice came from, as a compact tag ([`ModelSource`]).
    /// Paired with [`Self::model`].
    pub model_source: Option<ModelSource>,
    /// Reasoning/thinking effort — surfaced **only** when honestly recorded
    /// on a past event. Always `None` today (no event persists it); never
    /// inferred from the current config. See the module header.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Compact, honest label for the origin of an adapter selection — the
/// display-side projection of [`AdapterSelectionSource`]'s variant.
///
/// Kept as a small `Copy` enum (rather than reusing the event source, which
/// carries verbose provenance strings) so the renderer never leaks a full
/// config path into a one-glyph table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterSource {
    /// `cs tackle --adapter <flag>`.
    Cli,
    /// A formula step's `adapter = "<name>"` pin.
    Formula,
    /// The `$COSMON_DEFAULT_ADAPTER` environment variable.
    Env,
    /// The per-galaxy `.cosmon/config.toml`.
    Config,
    /// The global `~/.config/cosmon/config.toml`.
    Global,
    /// The built-in floor (no flag, no pin, no config).
    Default,
    /// Envelope-driven role resolution (reserved).
    Role,
}

impl AdapterSource {
    /// The compact tag shown in the table (`cli`, `formula`, `env`, …).
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Formula => "formula",
            Self::Env => "env",
            Self::Config => "config",
            Self::Global => "global",
            Self::Default => "default",
            Self::Role => "role",
        }
    }

    /// Project the verbose event source onto its compact display tag.
    #[must_use]
    pub fn from_event(src: &AdapterSelectionSource) -> Self {
        match src {
            AdapterSelectionSource::Cli { .. } => Self::Cli,
            AdapterSelectionSource::FormulaStep { .. } => Self::Formula,
            AdapterSelectionSource::EnvVar { .. } => Self::Env,
            AdapterSelectionSource::Config { .. } => Self::Config,
            AdapterSelectionSource::GlobalConfig { .. } => Self::Global,
            AdapterSelectionSource::Default { .. } => Self::Default,
            AdapterSelectionSource::EnvelopeRole { .. } => Self::Role,
        }
    }
}

/// Compact, honest label for the origin of a model selection — the
/// display-side projection of [`ModelSelectionSource`]'s variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// `cs tackle --model <id>`.
    Flag,
    /// A formula step's `model = "<id>"` pin.
    Formula,
    /// A model environment variable (`$COSMON_DEFAULT_MODEL` / `$ANTHROPIC_MODEL`).
    Env,
    /// The per-galaxy `.cosmon/config.toml` `default_model`.
    Config,
    /// The global config `default_model`.
    Global,
    /// The safe `None` floor (adapter's own default applied).
    Default,
}

impl ModelSource {
    /// The compact tag shown in the detail line (`flag`, `formula`, …).
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Formula => "formula",
            Self::Env => "env",
            Self::Config => "config",
            Self::Global => "global",
            Self::Default => "default",
        }
    }

    /// Project the verbose event source onto its compact display tag.
    #[must_use]
    pub fn from_event(src: &ModelSelectionSource) -> Self {
        match src {
            ModelSelectionSource::Flag { .. } => Self::Flag,
            ModelSelectionSource::FormulaPin { .. } => Self::Formula,
            ModelSelectionSource::EnvVar { .. } => Self::Env,
            ModelSelectionSource::Config { .. } => Self::Config,
            ModelSelectionSource::GlobalConfig { .. } => Self::Global,
            ModelSelectionSource::Default { .. } => Self::Default,
        }
    }
}

/// Placeholder rendered when nothing was recorded — a single ASCII hyphen so
/// the compact cell stays byte-safe for any downstream fixed-width surface.
pub const EMPTY_CELL: &str = "-";

impl AdapterAttribution {
    /// Fold an ordered slice of events into the honest attribution.
    ///
    /// The events must belong to a **single** molecule (the caller filters by
    /// `mol_id`) and be in append order (oldest first). The most recent
    /// [`EventV2::AdapterSelected`] / [`EventV2::ModelSelected`] wins — a
    /// re-tackle overwrites the earlier record, matching what actually ran.
    ///
    /// Only these two typed events are consulted. No other event, and no
    /// external config, contributes — this is what keeps the projection
    /// honest under the "never infer from current config" rule.
    #[must_use]
    pub fn fold<'a, I>(events: I) -> Self
    where
        I: IntoIterator<Item = &'a EventV2>,
    {
        let mut out = Self::default();
        for ev in events {
            match ev {
                EventV2::AdapterSelected {
                    adapter_name,
                    selection_source,
                    ..
                } => {
                    out.adapter = Some(adapter_name.clone());
                    out.adapter_source = Some(AdapterSource::from_event(selection_source));
                }
                EventV2::ModelSelected {
                    model,
                    selection_source,
                    ..
                } => {
                    out.model.clone_from(model);
                    out.model_source = Some(ModelSource::from_event(selection_source));
                }
                _ => {}
            }
        }
        out
    }

    /// `true` when no adapter selection was ever recorded — the row should
    /// render [`EMPTY_CELL`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.adapter.is_none()
    }

    /// Compact one-line rendering for the main table column.
    ///
    /// Shape: `adapter[/model] [source]`, plus `@effort` **only** when an
    /// effort was honestly recorded (never today). Examples:
    ///
    /// - `claude/claude-opus-4-8 [cli]`
    /// - `claude [config]` — adapter selected, model on the floor
    /// - `-` — nothing recorded (legacy / pending)
    ///
    /// The caller's column width clamps long model ids; this function does no
    /// truncation of its own so the same string serves a narrow TUI cell and
    /// a wide detail line identically.
    #[must_use]
    pub fn compact_cell(&self) -> String {
        let Some(adapter) = &self.adapter else {
            return EMPTY_CELL.to_string();
        };
        let mut s = adapter.clone();
        if let Some(model) = &self.model {
            s.push('/');
            s.push_str(model);
        }
        if let Some(src) = self.adapter_source {
            s.push_str(" [");
            s.push_str(src.tag());
            s.push(']');
        }
        if let Some(effort) = self.reasoning_effort {
            s.push('@');
            s.push_str(&effort.to_string());
        }
        s
    }

    /// Fuller, human-readable detail rendering for the expanded row.
    ///
    /// Names every axis explicitly (adapter, model, effort) with its source,
    /// so an operator expanding a row sees the full provenance. Effort renders
    /// as `-` (honest silence) whenever no event recorded it.
    #[must_use]
    pub fn detail_line(&self) -> String {
        let Some(adapter) = &self.adapter else {
            return format!("adapter: {EMPTY_CELL}");
        };
        let adapter_src = self.adapter_source.map_or("?", AdapterSource::tag);
        let model = self.model.as_deref().unwrap_or("(adapter default)");
        let model_src = self.model_source.map_or("floor", ModelSource::tag);
        let effort = self
            .reasoning_effort
            .map_or_else(|| EMPTY_CELL.to_string(), |e| e.to_string());
        format!(
            "adapter: {adapter} ({adapter_src})  model: {model} ({model_src})  effort: {effort}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::MoleculeId;
    use chrono::Utc;

    fn mid() -> MoleculeId {
        MoleculeId::new("task-20260712-6609").unwrap()
    }

    fn adapter_selected(name: &str, src: AdapterSelectionSource) -> EventV2 {
        EventV2::AdapterSelected {
            mol_id: mid(),
            adapter_name: name.to_string(),
            selected_at: Utc::now(),
            selection_source: src,
            role_hint: None,
            loop_ownership: Default::default(),
        }
    }

    fn model_selected(model: Option<&str>, src: ModelSelectionSource) -> EventV2 {
        EventV2::ModelSelected {
            mol_id: mid(),
            adapter_name: "claude".to_string(),
            model: model.map(ToString::to_string),
            selection_source: src,
            selected_at: Utc::now(),
        }
    }

    #[test]
    fn empty_fold_renders_placeholder() {
        let a = AdapterAttribution::fold(std::iter::empty());
        assert!(a.is_empty());
        assert_eq!(a.compact_cell(), "-");
        assert_eq!(a.detail_line(), "adapter: -");
    }

    #[test]
    fn folds_claude_cli_dispatch() {
        let events = vec![
            adapter_selected(
                "claude",
                AdapterSelectionSource::Cli {
                    flag: "claude".into(),
                },
            ),
            model_selected(
                Some("claude-opus-4-8"),
                ModelSelectionSource::Flag {
                    flag: "claude-opus-4-8".into(),
                },
            ),
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.adapter.as_deref(), Some("claude"));
        assert_eq!(a.adapter_source, Some(AdapterSource::Cli));
        assert_eq!(a.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(a.model_source, Some(ModelSource::Flag));
        assert_eq!(a.compact_cell(), "claude/claude-opus-4-8 [cli]");
    }

    #[test]
    fn most_recent_selection_wins() {
        let events = vec![
            adapter_selected(
                "local",
                AdapterSelectionSource::Default {
                    fallback_reason: "floor".into(),
                },
            ),
            adapter_selected(
                "claude",
                AdapterSelectionSource::Cli {
                    flag: "claude".into(),
                },
            ),
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.adapter.as_deref(), Some("claude"));
        assert_eq!(a.adapter_source, Some(AdapterSource::Cli));
    }

    #[test]
    fn model_floor_none_is_honest() {
        // A model selection with `None` (the safe floor) must NOT fabricate an
        // id — the cell shows the adapter alone.
        let events = vec![
            adapter_selected(
                "claude",
                AdapterSelectionSource::Config {
                    path: "/x/.cosmon/config.toml".into(),
                    key: "adapters.default".into(),
                },
            ),
            model_selected(
                None,
                ModelSelectionSource::Default {
                    fallback_reason: "no pin".into(),
                },
            ),
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.model, None);
        assert_eq!(a.compact_cell(), "claude [config]");
    }

    #[test]
    fn reasoning_effort_is_never_inferred() {
        // No event carries effort, so a fold NEVER surfaces one — the honesty
        // rule. This test pins that: the day an effort-carrying event lands,
        // whoever adds it must consciously update this assertion.
        let events = vec![adapter_selected(
            "claude",
            AdapterSelectionSource::Cli {
                flag: "claude".into(),
            },
        )];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.reasoning_effort, None);
        assert!(!a.compact_cell().contains('@'));
        assert!(a.detail_line().ends_with("effort: -"));
    }

    #[test]
    fn source_tags_are_compact() {
        assert_eq!(AdapterSource::Cli.tag(), "cli");
        assert_eq!(AdapterSource::Formula.tag(), "formula");
        assert_eq!(ModelSource::Flag.tag(), "flag");
        assert_eq!(ModelSource::Default.tag(), "default");
    }
}
