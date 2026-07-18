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
//! effort on any spawn-time event today (`crate::event_v2::AdapterSelected`
//! and `crate::event_v2::ModelSelected` carry no effort field), so a fold
//! always yields `None` for effort. It is **never** back-filled from the
//! current `.cosmon/config.toml` or a live `ModelSpec` — attributing today's
//! setting to yesterday's run would be a lie the operator cannot detect. The
//! field exists so that *if* a future event ever records the effort, the
//! whole pipeline surfaces it with no further change; until then, silence.

use crate::event_v2::{AdapterSelectionSource, EventV2, ModelSelectionSource};
use crate::model_spec::ReasoningEffort;

/// The **realized** model axis — what an adapter *actually* ran, folded from
/// [`EventV2::ModelObserved`] and **only** that event (delib-20260718-c70e).
///
/// The retrospective sibling of the intention axis ([`AdapterAttribution::model`]):
/// intention is the pin cosmon *chose* at spawn; realization is what the adapter
/// *reported* running. The two coexist — this axis never reads, and can never
/// clobber, the intention field (structural no-clobber: the fold arm that fills
/// this names only [`AdapterAttribution::realized`]).
///
/// It is a **tri-state**, not an `Option<String>` last-wins, because the feature
/// exists to reveal exactly the cases a flat slot fabricates
/// (`docs/design/realized-model/DECISIONS.md`, D1):
///
/// - a real Opus→Sonnet quota fallback is a *trajectory* of two models that both
///   ran — last-wins would collapse it into a single-model session that never
///   happened;
/// - a crashed worker that died before reporting ([`Self::Unknown`]) is not the
///   same as one that ran and stayed silent ([`Self::Silent`]) — `None` would
///   conflate them, and rendering `-` ("ran, said nothing") for a crash invents
///   an execution.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Realized {
    /// No observation, and no evidence the run completed — the worker died
    /// before reporting any model, or the molecule is legacy/pending. We
    /// genuinely do not know what ran. Rendered `?`. The honest default.
    #[default]
    Unknown,
    /// The dispatch ran to completion but never reported a concrete model id
    /// (an adapter that cannot surface it — codex/aider today). The *positive*
    /// claim "ran, said nothing". Rendered `-`.
    Silent,
    /// One or more concrete model ids were observed, in execution order with
    /// consecutive duplicates collapsed. A single element is a plain model; two
    /// or more is a *trajectory* (mid-session change / quota fallback), rendered
    /// `a->b`. Never empty (an empty observation is [`Self::Silent`]).
    Observed(Vec<String>),
}

impl Realized {
    /// The trajectory of observed ids, or `None` when nothing concrete was
    /// observed ([`Self::Unknown`] / [`Self::Silent`]).
    #[must_use]
    pub fn observed(&self) -> Option<&[String]> {
        match self {
            Self::Observed(ids) if !ids.is_empty() => Some(ids),
            _ => None,
        }
    }

    /// The honest one-glyph/one-fragment label for the detail line:
    /// `?` (unknown), `-` (silent), or the `a->b` trajectory (observed).
    #[must_use]
    pub fn detail_fragment(&self) -> String {
        match self {
            Self::Unknown => "?".to_string(),
            Self::Silent => EMPTY_CELL.to_string(),
            Self::Observed(ids) if ids.is_empty() => EMPTY_CELL.to_string(),
            Self::Observed(ids) => ids.join("->"),
        }
    }

    /// The parenthetical disposition tag for the detail line — how the value
    /// should be read: `unknown`, `silent`, or `observed`.
    #[must_use]
    pub fn disposition(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Silent => "silent",
            Self::Observed(ids) if ids.is_empty() => "silent",
            Self::Observed(_) => "observed",
        }
    }
}

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
    /// The **realized** model — what the adapter actually ran, folded from
    /// [`EventV2::ModelObserved`] and *only* that event. Coexists with the
    /// intention [`Self::model`] on a disjoint axis: the fold never reads the
    /// pin to fill this, so it can never fabricate a realization from an
    /// intention (delib-20260718-c70e; sibling of the reasoning-effort honesty
    /// rule). Defaults to [`Realized::Unknown`].
    pub realized: Realized,
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
    /// The **intention** axis (adapter/model) folds from `AdapterSelected` /
    /// `ModelSelected`; the **realized** axis folds from `ModelObserved` (and
    /// consults `MoleculeCompleted` only to tell a *silent* completed run apart
    /// from a *crashed* one). The two axes are disjoint: the realized arm names
    /// **only** [`Self::realized`] and never reads the pin, so no fold path can
    /// clobber intention with realization or fabricate one from the other. This
    /// is what keeps the projection honest under the "never infer" rule.
    #[must_use]
    pub fn fold<'a, I>(events: I) -> Self
    where
        I: IntoIterator<Item = &'a EventV2>,
    {
        let mut out = Self::default();
        // Realized axis — accumulated disjointly from the intention fields.
        let mut observed: Vec<String> = Vec::new();
        let mut ran_to_completion = false;
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
                EventV2::ModelObserved { model, .. } => {
                    // Realized trajectory: collapse consecutive duplicates so a
                    // stable session is one element and a quota fallback is two.
                    // This arm names ONLY the realized accumulator — never the
                    // intention fields — so no-clobber is structural.
                    if observed.last() != Some(model) {
                        observed.push(model.clone());
                    }
                }
                EventV2::MoleculeCompleted { .. } => {
                    ran_to_completion = true;
                }
                _ => {}
            }
        }
        // Resolve the tri-state from the disjoint accumulators. Observation
        // wins; else a completed-but-unobserved run is `Silent` ("ran, said
        // nothing"), and anything else — crashed, pending, legacy — is the
        // honest `Unknown`. The pin is NEVER consulted here.
        out.realized = if observed.is_empty() {
            if ran_to_completion {
                Realized::Silent
            } else {
                Realized::Unknown
            }
        } else {
            Realized::Observed(observed)
        };
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
    /// # Realized-model drift (delib-20260718-c70e, D3 — ASCII, drift-only)
    ///
    /// The compact cell shows the realized model **only when it drifts** from
    /// the pin — agreement is silence, drift is the signal. The realized
    /// segment uses ASCII sigils so the cell stays byte-safe for any
    /// fixed-width surface (the [`EMPTY_CELL`] discipline):
    ///
    /// - `claude/opus~>sonnet [cli]` — pinned `opus`, *realized* `sonnet`
    ///   (`~>` joins intention→realization);
    /// - `claude/opus~>opus->sonnet` collapses to `claude/opus~>sonnet` — a
    ///   trajectory whose head equals the pin drops the redundant head; a
    ///   genuine mid-realization change stays as `a->b` inside the segment;
    /// - `codex~>gpt-4o [config]` — no pin, but a model *was* observed (shown
    ///   without a leading `/` so it never reads as an intention pin);
    /// - `claude/opus [cli]` — realized **equals** the pin (agreement): no
    ///   glyph, byte-identical to the pre-realized rendering;
    /// - `claude/opus [cli]` — realized `Silent`/`Unknown`: the compact cell is
    ///   drift-*only*, so an unobserved run adds nothing here; the honest
    ///   `-`/`?` disposition lives in [`Self::detail_line`].
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
        if let Some(drift) = self.realized_drift() {
            s.push_str("~>");
            s.push_str(&drift);
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

    /// The realized trajectory to render *after* `~>` in the compact cell, or
    /// `None` when nothing should be shown (drift-only grammar): the realized
    /// axis is unobserved (`Silent`/`Unknown`), or it *agrees* with the pin.
    ///
    /// When the observed trajectory's head equals the pin, the redundant head
    /// is dropped so `pin~>head->tail` reads as `pin~>tail` — the drift arrow
    /// already carries "from the pin", so repeating it is noise.
    ///
    /// Public so a rich surface (the `cs peek` TUI) can paint the realized
    /// segment distinctly from the pin while sharing this one drift-computation
    /// with the plain [`Self::compact_cell`] — the two never diverge.
    #[must_use]
    pub fn realized_drift_display(&self) -> Option<String> {
        self.realized_drift()
    }

    /// See [`Self::realized_drift_display`]. Kept private so `compact_cell`'s
    /// callsite is byte-identical to the public accessor's.
    fn realized_drift(&self) -> Option<String> {
        let ids = self.realized.observed()?;
        let pin = self.model.as_deref();
        // Agreement: a single observed id equal to the pin → no glyph.
        if ids.len() == 1 && pin == Some(ids[0].as_str()) {
            return None;
        }
        // Drop a leading pin-equal head from a multi-step trajectory.
        let tail: &[String] = if ids.len() > 1 && pin == Some(ids[0].as_str()) {
            &ids[1..]
        } else {
            ids
        };
        Some(
            tail.iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("->"),
        )
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
        // Realized axis — the honest disposition of what actually ran, always
        // named explicitly so an unobserved run reads `? (unknown)` / `-
        // (silent)` rather than being confused with a confirmed match. Never
        // back-filled from the pin.
        let realized = self.realized.detail_fragment();
        let disposition = self.realized.disposition();
        format!(
            "adapter: {adapter} ({adapter_src})  model: {model} ({model_src})  \
             realized: {realized} ({disposition})  effort: {effort}"
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

    fn model_observed(model: &str) -> EventV2 {
        EventV2::ModelObserved {
            mol_id: mid(),
            adapter_name: "claude".to_string(),
            model: model.to_string(),
            observed_source:
                crate::model_realization::ModelObservationSource::ClaudeStreamJson,
            observed_at: Utc::now(),
        }
    }

    fn molecule_completed() -> EventV2 {
        EventV2::MoleculeCompleted {
            molecule_id: mid(),
            duration_ms: None,
            reason: "done".to_string(),
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

    // ---- Realized axis (delib-20260718-c70e) --------------------------------

    /// Case (a) — silent adapter: ran to completion, never reported a model.
    /// The tri-state is `Silent`, NOT an echo of the pin, and the compact cell
    /// stays drift-*only* (no realized glyph) while the detail names `- (silent)`.
    #[test]
    fn realized_silent_completed_run_never_echoes_pin() {
        let events = vec![
            adapter_selected(
                "codex",
                AdapterSelectionSource::Config {
                    path: "/x/.cosmon/config.toml".into(),
                    key: "adapters.default".into(),
                },
            ),
            model_selected(
                Some("gpt-5-codex"),
                ModelSelectionSource::Config {
                    path: "/x/.cosmon/config.toml".into(),
                    key: "adapters.codex.default_model".into(),
                },
            ),
            molecule_completed(),
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.realized, Realized::Silent);
        // Drift-only: a silent run adds no glyph to the compact cell.
        assert_eq!(a.compact_cell(), "codex/gpt-5-codex [config]");
        // The honesty lives in the detail line.
        assert!(a.detail_line().contains("realized: - (silent)"));
    }

    /// Case (b) — mid-session change: a real Opus→Sonnet quota fallback. Both
    /// models ran; the tri-state keeps the *trajectory*, never last-wins. The
    /// drift renders `pin~>tail` (the redundant pin-equal head is dropped).
    #[test]
    fn realized_mid_session_change_keeps_trajectory() {
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
            model_observed("claude-opus-4-8"),
            model_observed("claude-sonnet-5"),
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(
            a.realized,
            Realized::Observed(vec![
                "claude-opus-4-8".to_string(),
                "claude-sonnet-5".to_string(),
            ])
        );
        // Pin == head of trajectory → drop the head; show only the drift target.
        assert_eq!(a.compact_cell(), "claude/claude-opus-4-8~>claude-sonnet-5 [cli]");
        assert!(a
            .detail_line()
            .contains("realized: claude-opus-4-8->claude-sonnet-5 (observed)"));
    }

    /// Case (c) — worker dead before any observation: no `ModelObserved`, no
    /// completion. The tri-state is `Unknown` (`?`), distinct from `Silent`
    /// (`-`): rendering "ran, said nothing" for a crash would invent an
    /// execution. Compact cell stays drift-only.
    #[test]
    fn realized_dead_before_event_is_unknown_not_silent() {
        let events = vec![
            adapter_selected(
                "claude",
                AdapterSelectionSource::Cli {
                    flag: "claude".into(),
                },
            ),
            EventV2::WorkerExited {
                molecule_id: mid(),
                exit_code: Some(137),
                reason: "pane_died".into(),
            },
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.realized, Realized::Unknown);
        assert!(a.detail_line().contains("realized: ? (unknown)"));
        // Distinct from the silent disposition.
        assert_ne!(a.realized.detail_fragment(), EMPTY_CELL);
    }

    /// Drift-as-signal: when the realized id equals the pin, agreement is
    /// silence — the compact cell carries no realized glyph.
    #[test]
    fn realized_agreement_with_pin_renders_no_glyph() {
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
            model_observed("claude-opus-4-8"),
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.compact_cell(), "claude/claude-opus-4-8 [cli]");
        assert!(!a.compact_cell().contains("~>"));
        // But the detail still names it honestly as observed.
        assert!(a
            .detail_line()
            .contains("realized: claude-opus-4-8 (observed)"));
    }

    /// Observed without any pin (unpinned dispatch that still ran a concrete
    /// model): shown after the adapter with `~>` and NO leading `/`, so it can
    /// never be misread as an intention pin.
    #[test]
    fn realized_observed_without_pin_shows_drift_not_pin() {
        let events = vec![
            adapter_selected(
                "codex",
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
            model_observed("gpt-4o-2024-11-20"),
        ];
        let a = AdapterAttribution::fold(&events);
        assert_eq!(a.model, None);
        assert_eq!(a.compact_cell(), "codex~>gpt-4o-2024-11-20 [config]");
    }

    /// The realized fold reads ONLY `ModelObserved` — never the pin. A pinned
    /// dispatch with no observation must NOT surface the pin as realized: the
    /// structural no-clobber / never-back-fill guard (sibling of
    /// `reasoning_effort_is_never_inferred`).
    #[test]
    fn realized_is_never_backfilled_from_intention() {
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
        // Pin is present…
        assert_eq!(a.model.as_deref(), Some("claude-opus-4-8"));
        // …but realized was never observed, so it is Unknown, not the pin.
        assert_eq!(a.realized, Realized::Unknown);
        assert_eq!(a.realized.observed(), None);
    }

    /// Legacy fold (only intention events) stays byte-identical to the
    /// pre-realized rendering — the realized axis adds nothing when unobserved.
    #[test]
    fn legacy_intention_only_fold_is_byte_identical() {
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
        assert_eq!(a.compact_cell(), "claude/claude-opus-4-8 [cli]");
    }
}
