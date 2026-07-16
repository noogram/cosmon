// SPDX-License-Identifier: AGPL-3.0-only

//! Graceful-degradation matrix — which cosmon **verb-classes** stay
//! reliable on a weaker (open-weights, local) model, and which must
//! disable.
//!
//! This module is the *executable twin* of the doctrine table in
//! [ADR-118](../../../docs/adr/118-llmport-doctrine-and-degradation-matrix.md).
//! The ADR prose is the human-readable contract; this code is the
//! drift-resistant form: the unit tests below pin the 5×3 table so the
//! doc cannot silently rot away from the constant the system actually
//! reasons with.
//!
//! # Why this lives here and not behind a new trait
//!
//! The 2026-05-19 speculative-scaffolding rip
//! (chronicle `2026-05-19-w6-speculative-rip.md`) deleted the
//! `LlmProvider` trait because it was an empty closet: a port with no
//! one passing through it. The degradation query is deliberately *not*
//! a new trait, a new dispatch table, or a new `cs` verb. It is a pair
//! of pure inherent methods on the **already-live** [`Capabilities`]
//! type — siblings of [`Capabilities::can_fit`] — that answer a
//! question the operator genuinely poses: *"I am about to route a
//! request to this backend; which classes of cosmon work can I trust
//! it with?"*
//!
//! # The load-bearing row: `ControlPlane`
//!
//! The single most important fact the matrix encodes is that the cosmon
//! **control plane** — `nucleate`, `evolve`, `tackle`, `done`,
//! `observe`, `reconcile`, `tag`, `freeze` — consumes **zero LLM
//! tokens**. It is a typed state machine over JSON files on disk. So
//! [`VerbClass::ControlPlane`] is [`Reliability::Reliable`] at *every*
//! tier, including a hypothetical model so weak it cannot complete a
//! sentence. This is the compute-sovereignty floor: cosmon survives a
//! total frontier-API lockout for *orchestration*; only the **worker
//! cognition** inside a tackled molecule degrades. See ADR-118 §Context
//! (the Étienne Lempereur lockout question, corroborated by the Mensch
//! audition) and noogram ADR-049.

use serde::{Deserialize, Serialize};

use crate::capabilities::Capabilities;

/// A class of cosmon work, grouped by the *cognitive demand it places on
/// the underlying model*.
///
/// The taxonomy is coarse on purpose: it is a routing aid, not a
/// per-verb registry. Every LLM-touching cosmon feature maps onto
/// exactly one of these five buckets; the deterministic control plane
/// maps onto [`Self::ControlPlane`] and touches no model at all.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerbClass {
    /// Deterministic state-machine verbs: `nucleate`, `evolve`,
    /// `tackle`, `done`, `observe`, `reconcile`, `tag`, `freeze`.
    /// **No model is invoked.** Reliable even with no backend at all.
    ControlPlane,
    /// Single-shot free-text generation where *quality* degrades but
    /// *correctness* does not: a worker drafting a note, a one-shot
    /// summary, `cs ask`. A weak model produces a worse answer, not a
    /// broken one.
    FreeformGeneration,
    /// Output constrained to a machine-checkable shape (JSON, a tagged
    /// verdict, a tool call): session-route tier-2, `spec_audit`,
    /// triage classification. Reliable on a weak model **only** when
    /// decoding is grammar-constrained (GBNF) or tool-schema-bound;
    /// otherwise the model free-forms past the schema.
    StructuredExtraction,
    /// Long agentic loops with tool-use and self-correction: the
    /// `deep-think` panel, mission decomposition, the fleet-validator
    /// L1–L5 cascade. These compound errors across many turns and need
    /// a model that can hold a plan and call tools reliably.
    MultiStepAgentic,
    /// Operations whose prompt spans a large evidence set: review over
    /// a big diff, reconcile-with-summary, whole-corpus questions.
    /// Gated by the backend's context window — see
    /// [`Capabilities::can_fit`] for the per-request refinement.
    LongContext,
}

impl VerbClass {
    /// Every variant, for exhaustive table-building and tests.
    pub const ALL: [VerbClass; 5] = [
        VerbClass::ControlPlane,
        VerbClass::FreeformGeneration,
        VerbClass::StructuredExtraction,
        VerbClass::MultiStepAgentic,
        VerbClass::LongContext,
    ];
}

/// The strength bracket of a backend, derived purely from its advertised
/// [`Capabilities`] — never from a probe.
///
/// Ordering is meaningful: `Local < Mid < Frontier`. A class that is
/// [`Reliability::Reliable`] at `Mid` is reliable at `Frontier` too.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationTier {
    /// A small local model: short context, no advertised tool-calling
    /// or vision. Llama 3.2 3B / Mistral 7B class. The open-weights
    /// floor cosmon is designed to keep useful.
    Local,
    /// A capable open-weights model: tool-calling **or** a roomy
    /// context window. Qwen3 14–32B, Mixtral 8×7B, a generous Ollama
    /// deployment.
    Mid,
    /// A frontier hosted model: large context **and** robust
    /// tool-calling. Claude / GPT-4 class.
    Frontier,
}

/// How much a verb-class can be trusted on a given tier.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reliability {
    /// Use it without reservation.
    Reliable,
    /// Works, but with reduced quality or requiring a guard rail
    /// (GBNF grammar, smaller batch, retry). The operator should expect
    /// rougher output, not failure.
    Degraded,
    /// Do not route this class here — the model cannot be trusted to
    /// produce a usable result. The feature should disable rather than
    /// emit confident garbage.
    Unavailable,
}

/// Context floor (tokens) below which [`VerbClass::LongContext`] work is
/// not even worth attempting, independent of model intelligence.
const LONG_CONTEXT_FLOOR: u32 = 32_768;

/// Context ceiling that, together with tool-calling, marks the
/// [`DegradationTier::Frontier`] bracket.
const FRONTIER_CONTEXT_FLOOR: u32 = 100_000;

/// Context floor that, on its own, lifts a tool-less backend out of
/// [`DegradationTier::Local`] into [`DegradationTier::Mid`].
const MID_CONTEXT_FLOOR: u32 = 16_384;

/// The doctrine table, expressed once as a pure function of
/// `(tier, class)`.
///
/// This is the single source of truth the ADR-118 prose table mirrors.
/// It is `const`-evaluable and takes no `self`, so the unit tests can
/// pin every cell without constructing a [`Capabilities`].
#[must_use]
pub const fn reliability_at(tier: DegradationTier, class: VerbClass) -> Reliability {
    use DegradationTier::{Frontier, Local, Mid};
    use Reliability::{Degraded, Reliable, Unavailable};
    use VerbClass::{
        ControlPlane, FreeformGeneration, LongContext, MultiStepAgentic, StructuredExtraction,
    };

    match (class, tier) {
        // The control plane is deterministic — no model, always reliable.
        (ControlPlane, _) => Reliable,

        // Free-text: a weak model is worse, never wrong.
        (FreeformGeneration, Local) => Degraded,
        (FreeformGeneration, Mid | Frontier) => Reliable,

        // Constrained output: trustworthy on Local only behind a GBNF /
        // tool-schema guard rail, hence Degraded rather than Reliable.
        (StructuredExtraction, Local) => Degraded,
        (StructuredExtraction, Mid | Frontier) => Reliable,

        // Multi-turn agentic loops compound errors — disable on Local.
        (MultiStepAgentic, Local) => Unavailable,
        (MultiStepAgentic, Mid) => Degraded,
        (MultiStepAgentic, Frontier) => Reliable,

        // Long context is a window problem first; a small local window
        // simply cannot hold the prompt.
        (LongContext, Local) => Unavailable,
        (LongContext, Mid) => Degraded,
        (LongContext, Frontier) => Reliable,
    }
}

impl Capabilities {
    /// Classify this backend into a [`DegradationTier`] from its
    /// *advertised* capabilities alone — no I/O, no probe.
    ///
    /// The heuristic is deliberately simple and transparent:
    ///
    /// * **Frontier** — a large context window (`≥ 100k`) *and*
    ///   advertised tool-calling.
    /// * **Mid** — advertises tool-calling, *or* a context window
    ///   `≥ 16k`, but is not Frontier.
    /// * **Local** — neither: a small, tool-less model.
    ///
    /// Tier reflects what the adapter *advertises*. An operator running
    /// a 3B model behind an adapter that claims a 32k window will be
    /// classified `Mid`; honesty in the advertisement is the operator's
    /// lever (lower `max_context` for a genuinely small model).
    #[must_use]
    pub fn degradation_tier(&self) -> DegradationTier {
        if self.max_context >= FRONTIER_CONTEXT_FLOOR && self.supports_tools {
            DegradationTier::Frontier
        } else if self.supports_tools || self.max_context >= MID_CONTEXT_FLOOR {
            DegradationTier::Mid
        } else {
            DegradationTier::Local
        }
    }

    /// Reliability of a given [`VerbClass`] on this backend.
    ///
    /// Composes [`Self::degradation_tier`] with [`reliability_at`]. For
    /// [`VerbClass::LongContext`], the tier table is additionally
    /// floored by [`LONG_CONTEXT_FLOOR`]: a backend whose window cannot
    /// even reach the floor is [`Reliability::Unavailable`] regardless
    /// of how it scores on the other axes.
    #[must_use]
    pub fn reliability_for(&self, class: VerbClass) -> Reliability {
        let base = reliability_at(self.degradation_tier(), class);
        if matches!(class, VerbClass::LongContext) && self.max_context < LONG_CONTEXT_FLOOR {
            return Reliability::Unavailable;
        }
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(max_context: u32, supports_tools: bool, supports_vision: bool) -> Capabilities {
        Capabilities {
            max_context,
            supports_streaming: false,
            supports_tools,
            supports_vision,
            rate_limit_hint: None,
        }
    }

    #[test]
    fn control_plane_is_reliable_on_every_tier() {
        for tier in [
            DegradationTier::Local,
            DegradationTier::Mid,
            DegradationTier::Frontier,
        ] {
            assert_eq!(
                reliability_at(tier, VerbClass::ControlPlane),
                Reliability::Reliable,
                "control plane must never depend on model strength"
            );
        }
    }

    #[test]
    fn tier_ordering_is_local_lt_mid_lt_frontier() {
        assert!(DegradationTier::Local < DegradationTier::Mid);
        assert!(DegradationTier::Mid < DegradationTier::Frontier);
    }

    #[test]
    fn reliability_is_monotone_in_tier() {
        // For every class, strength never *lowers* reliability as the
        // tier rises (Unavailable < Degraded < Reliable ordering).
        fn rank(r: Reliability) -> u8 {
            match r {
                Reliability::Unavailable => 0,
                Reliability::Degraded => 1,
                Reliability::Reliable => 2,
            }
        }
        for class in VerbClass::ALL {
            let local = rank(reliability_at(DegradationTier::Local, class));
            let mid = rank(reliability_at(DegradationTier::Mid, class));
            let frontier = rank(reliability_at(DegradationTier::Frontier, class));
            assert!(local <= mid, "{class:?}: local must be ≤ mid");
            assert!(mid <= frontier, "{class:?}: mid must be ≤ frontier");
        }
    }

    #[test]
    fn small_toolless_model_is_local() {
        let c = caps(8_192, false, false);
        assert_eq!(c.degradation_tier(), DegradationTier::Local);
    }

    #[test]
    fn roomy_context_alone_lifts_to_mid() {
        // Mirrors the Ollama adapter's conservative 32k advertisement.
        let c = caps(32_768, false, false);
        assert_eq!(c.degradation_tier(), DegradationTier::Mid);
    }

    #[test]
    fn tools_alone_lift_to_mid_even_with_small_window() {
        let c = caps(8_192, true, false);
        assert_eq!(c.degradation_tier(), DegradationTier::Mid);
    }

    #[test]
    fn large_window_plus_tools_is_frontier() {
        let c = caps(200_000, true, true);
        assert_eq!(c.degradation_tier(), DegradationTier::Frontier);
    }

    #[test]
    fn large_window_without_tools_is_only_mid() {
        // Capability, not raw size, gates Frontier: a 200k window with
        // no tool-calling is still Mid.
        let c = caps(200_000, false, false);
        assert_eq!(c.degradation_tier(), DegradationTier::Mid);
    }

    #[test]
    fn local_disables_multistep_and_longcontext() {
        let c = caps(8_192, false, false);
        assert_eq!(
            c.reliability_for(VerbClass::MultiStepAgentic),
            Reliability::Unavailable
        );
        assert_eq!(
            c.reliability_for(VerbClass::LongContext),
            Reliability::Unavailable
        );
    }

    #[test]
    fn local_degrades_but_keeps_freeform_and_structured() {
        let c = caps(8_192, false, false);
        assert_eq!(
            c.reliability_for(VerbClass::FreeformGeneration),
            Reliability::Degraded
        );
        assert_eq!(
            c.reliability_for(VerbClass::StructuredExtraction),
            Reliability::Degraded
        );
        // Control plane survives total weakness.
        assert_eq!(
            c.reliability_for(VerbClass::ControlPlane),
            Reliability::Reliable
        );
    }

    #[test]
    fn long_context_floor_overrides_tier_table() {
        // A Mid-tier-by-tools backend with a tiny window still cannot do
        // LongContext work: the floor wins.
        let c = caps(8_192, true, false);
        assert_eq!(c.degradation_tier(), DegradationTier::Mid);
        assert_eq!(
            c.reliability_for(VerbClass::LongContext),
            Reliability::Unavailable,
            "below the {LONG_CONTEXT_FLOOR}-token floor, LongContext is unavailable regardless of tier"
        );
    }

    #[test]
    fn frontier_is_reliable_across_the_board() {
        let c = caps(200_000, true, true);
        for class in VerbClass::ALL {
            assert_eq!(
                c.reliability_for(class),
                Reliability::Reliable,
                "{class:?} should be reliable on a frontier backend"
            );
        }
    }
}
