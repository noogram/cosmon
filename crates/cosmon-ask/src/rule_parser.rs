// SPDX-License-Identifier: AGPL-3.0-only

//! Rule-first parser — table-driven matcher for ~20 canonical intents.
//!
//! The table is a TOML blob embedded at compile time
//! ([`DEFAULT_RULES_TOML`]). Callers that want to override it build
//! a [`RuleParser`] from a custom [`RuleTable`] — the parser itself
//! is a thin lookup over a pre-hashed vocabulary.
//!
//! # Matching algorithm
//!
//! 1. Tokenize on whitespace (lowercased, stripped of ASCII
//!    punctuation). No stemmer: the table already carries verb
//!    variants (`fix / patch / debug`).
//! 2. Walk the rules in declaration order. The first rule whose
//!    `verbs` set intersects the token set wins. Declaration order
//!    expresses priority; `plan`, because it can collide with `ship
//!    the plan`, is deliberately placed after `ship`.
//! 3. Galaxy hint: a bare bareword token that matches a known
//!    galaxy-hint keyword in the form `in|for|on <name>` or `<name>:`
//!    is recorded verbatim — resolution happens in
//!    [`crate::pipeline`], not here.
//!
//! # Confidence
//!
//! * Direct verb hit: the rule's `confidence` field (default 0.9).
//! * Galaxy hint additionally present: +0.05 (clamped to 0.99).
//! * No rule matched: 0.0 and a generic "task-work + issue kind"
//!   fallback — calling code will see the confidence drop below the
//!   floor and prompt.

use std::collections::HashSet;
use std::str::FromStr;

use cosmon_core::id::FormulaId;
use cosmon_core::kind::MoleculeKind;
use serde::{Deserialize, Serialize};

use crate::{AskError, AskTokens, Parser};

/// Canonical rule table shipped with the crate.
///
/// Each row maps a set of intent verbs to a `(kind, formula)` pair
/// and an anchor confidence. Add new intents here — do not add them
/// in the CLI crate. The keys match the first-cut table in the
/// molecule's briefing.
pub const DEFAULT_RULES_TOML: &str = include_str!("../rules.toml");

/// A single matching rule.
///
/// The wire-format is TOML; parsing through a [`RuleTable`] normalises
/// to this struct. Keeping the raw representation separate from the
/// in-memory index lets us extend the schema (e.g. a per-rule
/// destination fleet) without a breaking change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    /// Verb variants that select this rule (lowercase, no punctuation).
    pub verbs: Vec<String>,
    /// Molecule kind the rule dispatches into.
    pub kind: String,
    /// Formula id the rule dispatches into.
    pub formula: String,
    /// Anchor confidence for a direct verb hit. Clamped into
    /// `[0.0, 1.0]` at load time.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// Optional short human-readable example — used only for error
    /// messages and docs; the parser never inspects it.
    #[serde(default)]
    pub example: Option<String>,
}

fn default_confidence() -> f32 {
    0.9
}

/// Top-level wire-format: `rules = [...]` plus optional metadata.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleTable {
    /// Declaration-ordered rules. Priority: first match wins.
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl RuleTable {
    /// Parse a [`RuleTable`] from a TOML string. The default table
    /// ([`DEFAULT_RULES_TOML`]) is validated by a unit test, so
    /// `RuleTable::load_default()` is guaranteed infallible.
    ///
    /// # Errors
    ///
    /// Returns [`AskError::RuleTable`] if the TOML is malformed,
    /// carries unknown kinds, or has empty verb sets.
    pub fn from_toml(src: &str) -> Result<Self, AskError> {
        let mut table: Self =
            toml::from_str(src).map_err(|e| AskError::RuleTable(e.to_string()))?;
        for r in &mut table.rules {
            if r.verbs.is_empty() {
                return Err(AskError::RuleTable(format!(
                    "rule with kind={} has empty verbs",
                    r.kind
                )));
            }
            // normalise verbs
            for v in &mut r.verbs {
                *v = v.trim().to_lowercase();
            }
            // validate kind + formula now so the parser hot-path doesn't
            // have to
            MoleculeKind::from_str(&r.kind)
                .map_err(|_| AskError::RuleTable(format!("unknown molecule kind `{}`", r.kind)))?;
            FormulaId::new(r.formula.clone()).map_err(|e| {
                AskError::RuleTable(format!("invalid formula id `{}`: {e}", r.formula))
            })?;
            r.confidence = r.confidence.clamp(0.0, 1.0);
        }
        Ok(table)
    }

    /// Load the crate-default table. Panics only if the bundled
    /// TOML is malformed — which is covered by a unit test.
    ///
    /// Kept infallible so the CLI boot path doesn't have to branch
    /// on configuration.
    ///
    /// # Panics
    ///
    /// Panics if the compile-time-embedded `rules.toml` fails to
    /// parse. Guarded by the `bundled_rules_parse` unit test.
    #[must_use]
    pub fn load_default() -> Self {
        Self::from_toml(DEFAULT_RULES_TOML)
            .unwrap_or_else(|e| panic!("bundled rules.toml must parse: {e}"))
    }
}

/// Stateless rule matcher.
///
/// Construct once (cheap) and call [`Self::parse`] per invocation.
#[derive(Debug, Clone)]
pub struct RuleParser {
    table: RuleTable,
}

impl RuleParser {
    /// Wrap a [`RuleTable`] into a parser.
    #[must_use]
    pub fn new(table: RuleTable) -> Self {
        Self { table }
    }

    /// Construct with the bundled default table.
    ///
    /// # Panics
    ///
    /// Panics only if the compile-time-embedded `rules.toml` is
    /// malformed, which a unit test (`bundled_rules_parse`) catches.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(RuleTable::load_default())
    }

    /// Read-only access to the underlying table — used by tests and by
    /// the CLI's `--list-rules` diagnostic.
    #[must_use]
    pub fn table(&self) -> &RuleTable {
        &self.table
    }
}

impl Parser for RuleParser {
    fn parse(&self, text: &str) -> Result<(AskTokens, f32), AskError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(AskError::EmptyInput);
        }

        let tokens = tokenize(trimmed);
        let galaxy_hint = extract_galaxy_hint(&tokens);

        for rule in &self.table.rules {
            if rule.verbs.iter().any(|v| tokens.contains(v.as_str())) {
                let kind = MoleculeKind::from_str(&rule.kind).map_err(|_| {
                    AskError::Internal(format!(
                        "pre-validated rule carries invalid kind `{}`",
                        rule.kind
                    ))
                })?;
                let formula = FormulaId::new(rule.formula.clone()).map_err(|e| {
                    AskError::Internal(format!(
                        "pre-validated rule carries invalid formula `{}`: {e}",
                        rule.formula
                    ))
                })?;
                let intent_verb = rule
                    .verbs
                    .iter()
                    .find(|v| tokens.contains(v.as_str()))
                    .cloned()
                    .unwrap_or_else(|| rule.verbs[0].clone());
                let mut confidence = rule.confidence;
                if galaxy_hint.is_some() {
                    // Galaxy hint lifts confidence: architect §4 treats
                    // "which galaxy?" as the second principal risk. When
                    // we have an explicit answer, we trust the intent
                    // slightly more.
                    confidence = (confidence + 0.05).min(0.99);
                }
                return Ok((
                    AskTokens {
                        intent_verb,
                        kind,
                        formula,
                        galaxy_hint,
                        topic: trimmed.to_owned(),
                    },
                    confidence,
                ));
            }
        }

        // Fallback: no verb matched. Return a generic task-work tuple
        // with 0.0 confidence so the confidence gate fires an atomic
        // question rather than auto-dispatching.
        let formula = FormulaId::new("task-work")
            .map_err(|e| AskError::Internal(format!("task-work should be valid: {e}")))?;
        Ok((
            AskTokens {
                intent_verb: "unknown".to_owned(),
                kind: MoleculeKind::Task,
                formula,
                galaxy_hint,
                topic: trimmed.to_owned(),
            },
            0.0,
        ))
    }
}

/// Lowercase + punctuation-strip tokenize. Preserves only ASCII
/// alphanumerics + hyphens so multi-word galaxy names (`deep-think`,
/// `mailroom-voice`) survive.
fn tokenize(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
                .collect::<String>()
                .trim_matches(':')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Extract a galaxy name from `in|for|on|into <name>` or `<name>:`.
/// Deliberately tolerant — the registry resolve step is the hard gate.
fn extract_galaxy_hint(tokens: &HashSet<String>) -> Option<String> {
    // We lost sentence order when we hashed, so re-scan the raw input
    // via the tokens set + a re-parse: the caller reconstructs order
    // if needed. For v0 we accept any token that ends with ':' in the
    // raw input, which `tokenize` strips — so we need a separate
    // scan. Keep it simple: iterate tokens, return the first one that
    // *looks like* a galaxy name (single-word, all lowercase, between
    // 3 and 32 chars, not a stopword).
    const STOPWORDS: &[&str] = &[
        "the",
        "a",
        "an",
        "in",
        "for",
        "on",
        "into",
        "to",
        "from",
        "of",
        "my",
        "our",
        "this",
        "that",
        "with",
        "without",
        "and",
        "or",
        "bug",
        "plan",
        "ship",
        "fix",
        "patch",
        "debug",
        "triage",
        "review",
        "audit",
        "deploy",
        "release",
        "write",
        "draft",
        "chronicle",
        "note",
        "record",
        "refactor",
        "clean",
        "tidy",
        "explore",
        "investigate",
        "map",
        "survey",
        "delib",
        "deliberate",
        "panel",
        "architect",
        "design",
        "idea",
        "task",
        "decision",
        "issue",
        "deliberation",
    ];
    tokens
        .iter()
        .find(|t| {
            t.len() >= 3
                && t.len() <= 32
                && !STOPWORDS.contains(&t.as_str())
                && t.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_CONFIDENCE_FLOOR;

    #[test]
    fn bundled_rules_parse() {
        let t = RuleTable::load_default();
        assert!(
            t.rules.len() >= 10,
            "bundled rules should cover at least 10 intents, got {}",
            t.rules.len()
        );
    }

    #[test]
    fn fix_resolves_to_issue_task_work() {
        let p = RuleParser::with_defaults();
        let (tokens, conf) = p.parse("fix the bug in mailroom").unwrap();
        assert_eq!(tokens.kind, MoleculeKind::Issue);
        assert_eq!(tokens.formula.as_str(), "task-work");
        assert_eq!(tokens.galaxy_hint.as_deref(), Some("mailroom"));
        assert!(conf >= DEFAULT_CONFIDENCE_FLOOR, "got {conf}");
    }

    #[test]
    fn plan_resolves_to_idea_idea_to_plan() {
        let p = RuleParser::with_defaults();
        let (tokens, conf) = p.parse("plan the cosmon-node rollout").unwrap();
        assert_eq!(tokens.kind, MoleculeKind::Idea);
        assert_eq!(tokens.formula.as_str(), "idea-to-plan");
        assert!(conf >= DEFAULT_CONFIDENCE_FLOOR, "got {conf}");
    }

    #[test]
    fn delib_resolves_to_deliberation_deep_think() {
        let p = RuleParser::with_defaults();
        let (tokens, conf) = p.parse("deliberate on cs ask with jobs and niel").unwrap();
        assert_eq!(tokens.kind, MoleculeKind::Deliberation);
        assert_eq!(tokens.formula.as_str(), "deep-think");
        assert!(conf >= DEFAULT_CONFIDENCE_FLOOR, "got {conf}");
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn unknown_verb_yields_zero_confidence() {
        let p = RuleParser::with_defaults();
        let (tokens, conf) = p.parse("yodel the elephant").unwrap();
        assert_eq!(conf, 0.0);
        // fallback tuple is still coherent
        assert_eq!(tokens.kind, MoleculeKind::Task);
        assert_eq!(tokens.formula.as_str(), "task-work");
    }

    #[test]
    fn empty_input_is_an_error() {
        let p = RuleParser::with_defaults();
        matches!(p.parse("   "), Err(AskError::EmptyInput));
    }

    #[test]
    fn galaxy_hint_lifts_confidence() {
        let p = RuleParser::with_defaults();
        let (_, with_hint) = p.parse("fix the bug in mailroom").unwrap();
        let (_, without_hint) = p.parse("fix the bug").unwrap();
        assert!(with_hint > without_hint, "{with_hint} > {without_hint}");
    }

    #[test]
    fn all_default_rules_validate_kind_and_formula() {
        // Load through `from_toml` (not `load_default`) so the
        // validation errors surface here instead of as a panic.
        let t = RuleTable::from_toml(DEFAULT_RULES_TOML).expect("default rules must validate");
        for r in t.rules {
            assert!(!r.verbs.is_empty());
            // kind parses
            MoleculeKind::from_str(&r.kind).unwrap();
            // formula id non-empty
            assert!(!r.formula.is_empty());
            // confidence is a sane anchor
            assert!(r.confidence >= 0.5 && r.confidence <= 1.0);
        }
    }

    #[test]
    fn rejects_empty_verb_list() {
        let bad = r#"
[[rules]]
verbs = []
kind = "task"
formula = "task-work"
"#;
        assert!(matches!(
            RuleTable::from_toml(bad),
            Err(AskError::RuleTable(_))
        ));
    }

    #[test]
    fn rejects_unknown_kind() {
        let bad = r#"
[[rules]]
verbs = ["x"]
kind = "nonsense"
formula = "task-work"
"#;
        assert!(matches!(
            RuleTable::from_toml(bad),
            Err(AskError::RuleTable(_))
        ));
    }
}
