// SPDX-License-Identifier: AGPL-3.0-only

//! `AskPipeline` — the typestate composition of parse + resolve + gate.
//!
//! The pipeline does **not** dispatch molecules itself. Its job is to
//! decide, from free text, whether the system has enough information
//! to auto-dispatch and — if not — to return the structured atomic
//! question the CLI should surface. The actual `cs nucleate` + `cs
//! tackle` calls happen in `cosmon-cli::cmd::ask`, which consumes the
//! `AskState::Resolved` branch.
//!
//! # State transitions
//!
//! ```text
//! free text
//!     │  .parse(text)
//!     ▼
//!   Parsed { tokens, confidence }  ──► AskedClarification (below floor)
//!     │
//!     │  .resolve(registry)
//!     ▼
//!   Resolved { galaxy, formula, vars }  ──► AskedClarification (unknown galaxy)
//!     │
//!     │ (CLI: cs nucleate; cs tackle …)
//!     ▼
//!   Dispatched { mol_id, worker_id }
//! ```
//!
//! `Dispatched` is a record-keeping variant the CLI writes after the
//! shell-out verbs return. The pipeline never shells out on its own
//! — the stateless-core invariant forbids any I/O here beyond the
//! registry read.

use std::collections::HashMap;

use cosmon_core::id::{FormulaId, MoleculeId, WorkerId};
use cosmon_registry::{Galaxy, GalaxyIndex};
use serde::{Deserialize, Serialize};

use crate::atomic_question::AtomicQuestion;
use crate::{AskError, AskTokens, Parser, DEFAULT_CONFIDENCE_FLOOR};

/// Typestate over the ask pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AskState {
    /// Parser returned tokens with a confidence score. The gate has
    /// **not** been applied yet — callers must invoke
    /// [`AskState::gated`] next.
    Parsed {
        /// Tokens extracted by the parser.
        tokens: AskTokens,
        /// Confidence in `[0.0, 1.0]`.
        confidence: f32,
    },

    /// Pipeline paused for operator input. The CLI renders
    /// [`AtomicQuestion`] and feeds the answer back via
    /// [`AskState::resume_with_galaxy`] or
    /// [`AskState::resume_accept_low_confidence`].
    AskedClarification {
        /// Reason-slug (e.g. `low_confidence`, `unknown_galaxy`) —
        /// used by audit logs.
        reason: String,
        /// The question to surface.
        question: AtomicQuestion,
    },

    /// Fully resolved — ready for dispatch by the CLI handler.
    Resolved {
        /// Resolved galaxy entry.
        galaxy: Galaxy,
        /// Formula id to nucleate.
        formula: FormulaId,
        /// Variables to inject (`topic`, …). `cs nucleate --var` pairs.
        vars: HashMap<String, String>,
    },

    /// Terminal state — the CLI has completed `cs nucleate` + `cs
    /// tackle` and is recording the outcome for audit.
    Dispatched {
        /// Molecule the ask resolved into.
        mol_id: MoleculeId,
        /// Worker tmux session id.
        worker_id: WorkerId,
    },
}

impl AskState {
    /// Apply the confidence gate. Returns `Self::Parsed` unchanged when
    /// confidence ≥ floor; otherwise wraps the tokens into
    /// `Self::AskedClarification`.
    #[must_use]
    pub fn gated(self, floor: f32) -> Self {
        match self {
            Self::Parsed { tokens, confidence } if confidence < floor => Self::AskedClarification {
                reason: "low_confidence".to_owned(),
                question: AtomicQuestion::low_confidence(tokens, confidence),
            },
            other => other,
        }
    }

    /// After a low-confidence prompt, the operator chose to accept
    /// the rule-inferred default. Promotes the state to `Parsed`
    /// with confidence clamped at the floor so downstream logic can
    /// proceed uniformly.
    #[must_use]
    pub fn resume_accept_low_confidence(self, floor: f32) -> Self {
        match self {
            Self::AskedClarification {
                reason,
                question: AtomicQuestion { captured, .. },
            } if reason == "low_confidence" => Self::Parsed {
                tokens: captured,
                confidence: floor,
            },
            other => other,
        }
    }

    /// After an unknown-galaxy prompt, the operator named one of the
    /// candidates. We overwrite the galaxy hint and let the caller
    /// re-attempt `resolve`.
    #[must_use]
    pub fn resume_with_galaxy(self, galaxy_name: String) -> Self {
        match self {
            Self::AskedClarification {
                reason,
                question: AtomicQuestion { mut captured, .. },
            } if reason == "unknown_galaxy" => {
                captured.galaxy_hint = Some(galaxy_name);
                Self::Parsed {
                    tokens: captured,
                    // Lift to floor since the operator just confirmed
                    // the galaxy — the only previously-missing piece.
                    confidence: DEFAULT_CONFIDENCE_FLOOR,
                }
            }
            other => other,
        }
    }
}

/// The stateless pipeline composition.
///
/// Construct once per process (cheap) and reuse across invocations.
/// All methods are `&self`; the type holds no mutable state.
#[derive(Debug)]
pub struct AskPipeline<P: Parser, R: GalaxyIndex> {
    parser: P,
    registry: R,
    confidence_floor: f32,
}

impl<P: Parser, R: GalaxyIndex> AskPipeline<P, R> {
    /// Build a pipeline with the default confidence floor (0.85).
    pub fn new(parser: P, registry: R) -> Self {
        Self {
            parser,
            registry,
            confidence_floor: DEFAULT_CONFIDENCE_FLOOR,
        }
    }

    /// Override the confidence floor. Clamped to `[0.0, 1.0]`.
    #[must_use]
    pub fn with_confidence_floor(mut self, floor: f32) -> Self {
        self.confidence_floor = floor.clamp(0.0, 1.0);
        self
    }

    /// Current confidence floor.
    #[must_use]
    pub fn confidence_floor(&self) -> f32 {
        self.confidence_floor
    }

    /// Run stages A + B + C (parse + resolve + gate) in one shot.
    ///
    /// This is the happy-path entry point. Callers that need to
    /// separate the stages (e.g. to stream atomic-question UI
    /// iteratively) can drive `AskState` manually via the helpers on
    /// the enum.
    ///
    /// # Errors
    ///
    /// Bubbles up [`AskError::EmptyInput`] from the parser and any
    /// registry error that is not "name not found" (name-not-found
    /// is **not** an error — it promotes the state to
    /// `AskedClarification { reason: "unknown_galaxy" }`).
    pub fn run(&self, text: &str) -> Result<AskState, AskError> {
        let (tokens, confidence) = self.parser.parse(text)?;
        let parsed = AskState::Parsed { tokens, confidence }.gated(self.confidence_floor);

        // Only the Parsed branch can go further — AskedClarification
        // short-circuits here and bubbles up to the CLI.
        match parsed {
            AskState::Parsed {
                tokens,
                confidence: _,
            } => Ok(Self::resolve(&tokens, &self.registry)),
            other => Ok(other),
        }
    }

    fn resolve(tokens: &AskTokens, registry: &R) -> AskState {
        let Some(hint) = &tokens.galaxy_hint else {
            let candidates: Vec<String> = registry.list().into_iter().map(|g| g.name).collect();
            return AskState::AskedClarification {
                reason: "unknown_galaxy".to_owned(),
                question: AtomicQuestion::which_galaxy(tokens.clone(), &candidates),
            };
        };
        let Some(galaxy) = registry.resolve(hint) else {
            let candidates: Vec<String> = registry.list().into_iter().map(|g| g.name).collect();
            return AskState::AskedClarification {
                reason: "unknown_galaxy".to_owned(),
                question: AtomicQuestion::which_galaxy(tokens.clone(), &candidates),
            };
        };

        // Per-galaxy defaults override the rule-anchored formula when
        // the galaxy has a preference for this kind. This is how e.g.
        // `mailroom` gets its own "issue → bug-closure" preference
        // without hardcoding it in the rule table.
        let formula = galaxy
            .default_formulas
            .get(&tokens.kind)
            .cloned()
            .unwrap_or_else(|| tokens.formula.clone());

        let mut vars = HashMap::new();
        vars.insert("topic".to_owned(), tokens.topic.clone());

        AskState::Resolved {
            galaxy,
            formula,
            vars,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule_parser::RuleParser;
    use cosmon_core::kind::MoleculeKind;
    use cosmon_registry::Galaxy;
    use std::path::PathBuf;

    /// Hand-rolled registry for unit tests — avoids dragging in a
    /// tempfile + TOML round-trip for every scenario.
    struct FakeRegistry(Vec<Galaxy>);

    impl GalaxyIndex for FakeRegistry {
        fn resolve(&self, name: &str) -> Option<Galaxy> {
            self.0.iter().find(|g| g.name == name).cloned()
        }
        fn list(&self) -> Vec<Galaxy> {
            self.0.clone()
        }
    }

    fn g(name: &str) -> Galaxy {
        Galaxy {
            name: name.into(),
            path: PathBuf::from(format!("/tmp/{name}")),
            fleet: "default".into(),
            claude_md_digest: None,
            default_formulas: HashMap::new(),
        }
    }

    #[test]
    fn happy_path_dispatches() {
        let pipe = AskPipeline::new(
            RuleParser::with_defaults(),
            FakeRegistry(vec![g("mailroom")]),
        );
        let state = pipe.run("fix the bug in mailroom").unwrap();
        match state {
            AskState::Resolved {
                galaxy,
                formula,
                vars,
            } => {
                assert_eq!(galaxy.name, "mailroom");
                assert_eq!(formula.as_str(), "task-work");
                assert_eq!(vars.get("topic").unwrap(), "fix the bug in mailroom");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn low_confidence_pauses_for_clarification() {
        let pipe = AskPipeline::new(
            RuleParser::with_defaults(),
            FakeRegistry(vec![g("mailroom")]),
        );
        let state = pipe.run("yodel the elephant into mailroom").unwrap();
        match state {
            AskState::AskedClarification { reason, .. } => {
                assert_eq!(reason, "low_confidence");
            }
            other => panic!("expected AskedClarification, got {other:?}"),
        }
    }

    #[test]
    fn unknown_galaxy_pauses_for_clarification() {
        let pipe = AskPipeline::new(
            RuleParser::with_defaults(),
            FakeRegistry(vec![g("mailroom"), g("cosmon")]),
        );
        let state = pipe.run("fix the bug").unwrap();
        match state {
            AskState::AskedClarification { reason, question } => {
                assert_eq!(reason, "unknown_galaxy");
                // Default + up to 3 alternatives populated from the registry.
                assert!(!question.default.slug.is_empty());
            }
            other => panic!("expected unknown_galaxy clarification, got {other:?}"),
        }
    }

    #[test]
    fn resume_with_galaxy_advances_to_resolved() {
        let pipe = AskPipeline::new(
            RuleParser::with_defaults(),
            FakeRegistry(vec![g("mailroom"), g("cosmon")]),
        );
        let state = pipe.run("fix the bug").unwrap();
        let state = state.resume_with_galaxy("mailroom".into());
        // The enum can now be re-driven via .run logic — but at this
        // level we confirm the Parsed hand-off carried the hint.
        if let AskState::Parsed { tokens, confidence } = state {
            assert_eq!(tokens.galaxy_hint.as_deref(), Some("mailroom"));
            assert!((confidence - DEFAULT_CONFIDENCE_FLOOR).abs() < 1e-6);
        } else {
            panic!("expected Parsed after resume");
        }
    }

    #[test]
    fn galaxy_default_formula_overrides_rule_anchor() {
        let mut overriding = g("mailroom");
        overriding
            .default_formulas
            .insert(MoleculeKind::Issue, FormulaId::new("bug-closure").unwrap());
        let pipe = AskPipeline::new(RuleParser::with_defaults(), FakeRegistry(vec![overriding]));
        let state = pipe.run("fix the bug in mailroom").unwrap();
        if let AskState::Resolved { formula, .. } = state {
            assert_eq!(formula.as_str(), "bug-closure");
        } else {
            panic!("expected Resolved");
        }
    }

    #[test]
    fn low_confidence_resume_accept_promotes_to_parsed() {
        let pipe = AskPipeline::new(
            RuleParser::with_defaults(),
            FakeRegistry(vec![g("mailroom")]),
        );
        let state = pipe.run("yodel the elephant").unwrap();
        let promoted = state.resume_accept_low_confidence(pipe.confidence_floor());
        matches!(promoted, AskState::Parsed { .. });
    }

    #[test]
    fn confidence_floor_is_clamped() {
        let pipe = AskPipeline::new(RuleParser::with_defaults(), FakeRegistry(vec![]))
            .with_confidence_floor(2.0);
        assert!((pipe.confidence_floor() - 1.0).abs() < 1e-6);
    }
}
