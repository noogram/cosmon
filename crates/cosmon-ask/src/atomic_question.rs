// SPDX-License-Identifier: AGPL-3.0-only

//! Atomic question — the pipeline's escape valve when confidence is
//! below the floor or the galaxy cannot be resolved.
//!
//! The discipline is the operator's "one question, one decision" rule
//! (see `feedback_one_question_one_decision` in global memory): every
//! prompt surfaces a single decision, a default, and named
//! alternatives. The CLI layer renders this into the familiar
//! `1) default  2) …  later` verdict-door; this crate only carries
//! the data structure.

use serde::{Deserialize, Serialize};

use crate::AskTokens;

/// A structured, atomic clarification.
///
/// Preserves the operator's intent tokens so the CLI can re-display
/// the original free text alongside the numbered choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtomicQuestion {
    /// Prompt text shown to the operator (e.g. "which galaxy?").
    pub prompt: String,

    /// Default choice. Rendered as `1`. Pressing enter selects it.
    pub default: Choice,

    /// 0–3 named alternatives. The panel (architect §4) caps the
    /// list at a handful of options to preserve the atomic shape.
    pub alternatives: Vec<Choice>,

    /// Tokens captured up to this point — the CLI echoes them so the
    /// operator sees the parse in context.
    pub captured: AskTokens,
}

/// One verdict-door choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// Short slug used on the wire when the operator answers in
    /// `--answer <slug>` mode (scripting or automation).
    pub slug: String,
    /// Human-readable label rendered in the UI.
    pub label: String,
}

impl AtomicQuestion {
    /// Build an atomic question for the "which galaxy?" scenario.
    ///
    /// `candidates` is the registry's list of galaxy names. The first
    /// element becomes the default; the remaining (up to three) are
    /// alternatives; extras are folded into a single "other"
    /// fallback slug so the rendered prompt stays scannable.
    #[must_use]
    pub fn which_galaxy(captured: AskTokens, candidates: &[String]) -> Self {
        let (default, alternatives) = split_default_and_alternatives(candidates);
        Self {
            prompt: "Which galaxy should I dispatch to?".to_owned(),
            default,
            alternatives,
            captured,
        }
    }

    /// Build an atomic question for the "confidence too low" scenario.
    ///
    /// Offers the matched (kind, formula) as the default and two
    /// named alternatives the operator can type in full later.
    #[must_use]
    pub fn low_confidence(captured: AskTokens, confidence: f32) -> Self {
        Self {
            prompt: format!(
                "Intent unclear (confidence {confidence:.2}). Continue with the rule-inferred default?"
            ),
            default: Choice {
                slug: "accept".to_owned(),
                label: format!(
                    "Dispatch as {kind}/{formula}",
                    kind = captured.kind,
                    formula = captured.formula
                ),
            },
            alternatives: vec![
                Choice {
                    slug: "rewrite".to_owned(),
                    label: "Let me rewrite the prompt".to_owned(),
                },
                Choice {
                    slug: "abort".to_owned(),
                    label: "Abort — do nothing".to_owned(),
                },
            ],
            captured,
        }
    }

    /// Build the over-quota refusal when too many workers are running.
    /// Architect §4 specified `queue | override | abort`.
    #[must_use]
    pub fn running_quota(captured: AskTokens, running: usize) -> Self {
        Self {
            prompt: format!(
                "{running} workers already running. `cs ensemble --running` ≥ 3 triggers a gate."
            ),
            default: Choice {
                slug: "queue".to_owned(),
                label: "Queue — park as temp:warm, do not dispatch yet".to_owned(),
            },
            alternatives: vec![
                Choice {
                    slug: "override".to_owned(),
                    label: "Override — dispatch anyway".to_owned(),
                },
                Choice {
                    slug: "abort".to_owned(),
                    label: "Abort — do nothing".to_owned(),
                },
            ],
            captured,
        }
    }
}

fn split_default_and_alternatives(candidates: &[String]) -> (Choice, Vec<Choice>) {
    if candidates.is_empty() {
        return (
            Choice {
                slug: "register".to_owned(),
                label: "No galaxies registered. Register one via ~/.config/cosmon/galaxies.toml."
                    .to_owned(),
            },
            Vec::new(),
        );
    }
    let mut iter = candidates.iter().cloned();
    let default_name = iter.next().unwrap_or_default();
    let default = Choice {
        slug: default_name.clone(),
        label: default_name,
    };
    let alternatives: Vec<Choice> = iter
        .take(3)
        .map(|name| Choice {
            slug: name.clone(),
            label: name,
        })
        .collect();
    (default, alternatives)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::FormulaId;
    use cosmon_core::kind::MoleculeKind;

    fn toks() -> AskTokens {
        AskTokens {
            intent_verb: "fix".into(),
            kind: MoleculeKind::Issue,
            formula: FormulaId::new("task-work").unwrap(),
            galaxy_hint: None,
            topic: "fix the bug".into(),
        }
    }

    #[test]
    fn which_galaxy_with_three_candidates() {
        let q = AtomicQuestion::which_galaxy(
            toks(),
            &["cosmon".into(), "mailroom".into(), "earshot".into()],
        );
        assert_eq!(q.default.slug, "cosmon");
        assert_eq!(q.alternatives.len(), 2);
    }

    #[test]
    fn which_galaxy_empty_still_yields_a_default() {
        let q = AtomicQuestion::which_galaxy(toks(), &[]);
        assert_eq!(q.default.slug, "register");
    }

    #[test]
    fn low_confidence_offers_rewrite_and_abort() {
        let q = AtomicQuestion::low_confidence(toks(), 0.4);
        assert!(q.alternatives.iter().any(|c| c.slug == "rewrite"));
        assert!(q.alternatives.iter().any(|c| c.slug == "abort"));
    }

    #[test]
    fn running_quota_offers_queue_override_abort() {
        let q = AtomicQuestion::running_quota(toks(), 3);
        assert_eq!(q.default.slug, "queue");
        let slugs: Vec<_> = q.alternatives.iter().map(|c| c.slug.as_str()).collect();
        assert!(slugs.contains(&"override"));
        assert!(slugs.contains(&"abort"));
    }
}
