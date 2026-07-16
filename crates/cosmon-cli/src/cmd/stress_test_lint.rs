// SPDX-License-Identifier: AGPL-3.0-only

//! ADR-085 §M5 — `delib-prep` keyword lint.
//!
//! Static detector for the canonical phrases that signal a `deep-think`
//! deliberation is operating as a stress-test of a pre-committed prior.
//! When the operator nucleates `deep-think` with a question that mentions
//! one of these phrases but did not declare `--class stress-test`, we warn
//! (not error — the operator can override): either the class declaration
//! is missing, or the keyword should be removed. Either way, the
//! mismatch is the audit-trail signal.
//!
//! Why a static enum and not a regex list: the canonical set is
//! load-bearing — it is named verbatim in [ADR-085](../../../../docs/adr/085-stress-test-seal-mechanism.md)
//! §6 *Risks named*. A typed enum makes the maintenance contract
//! explicit (`StressTestKeyword::ALL` is the source of truth) and lets
//! callers exhaustively match if they later need to surface *which*
//! keyword fired.

use cosmon_core::molecule_class::MoleculeClass;

/// Canonical phrases that mark a `deep-think` framing as stress-test class.
///
/// Per ADR-085 §6 risk #1 (*keyword evasion*): this set is intentionally
/// small and verbatim. New phrases require an ADR amendment, not a silent
/// PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StressTestKeyword {
    /// Bare *"stress-test"*.
    StressTest,
    /// *"meta-frame"* — the deliberation re-frames an earlier framing.
    MetaFrame,
    /// *"disconfirming observation"* — Janis §3 predicate.
    DisconfirmingObservation,
    /// *"falsification"* — Popper-flavoured self-test.
    Falsification,
    /// *"pre-commitment"* — operator pre-committed prior, ADR-085 §1.
    PreCommitment,
}

impl StressTestKeyword {
    /// Every canonical keyword. Used by the detector and by tests to lock
    /// the set against silent additions.
    pub const ALL: [Self; 5] = [
        Self::StressTest,
        Self::MetaFrame,
        Self::DisconfirmingObservation,
        Self::Falsification,
        Self::PreCommitment,
    ];

    /// Canonical lowercase phrase as it appears in ADR-085.
    #[must_use]
    pub const fn canonical_phrase(self) -> &'static str {
        match self {
            Self::StressTest => "stress-test",
            Self::MetaFrame => "meta-frame",
            Self::DisconfirmingObservation => "disconfirming observation",
            Self::Falsification => "falsification",
            Self::PreCommitment => "pre-commitment",
        }
    }
}

/// Scan `text` for the canonical stress-test phrases, case-insensitive.
/// Returns the matched keywords in canonical order, deduplicated.
#[must_use]
pub fn detect_keywords(text: &str) -> Vec<StressTestKeyword> {
    let haystack = text.to_ascii_lowercase();
    StressTestKeyword::ALL
        .iter()
        .copied()
        .filter(|kw| haystack.contains(kw.canonical_phrase()))
        .collect()
}

/// Format the operator-facing warning when keywords are detected without
/// `--class stress-test`. Stable string — locked by tests.
fn format_warning(matched: &[StressTestKeyword]) -> String {
    let phrases: Vec<&'static str> = matched.iter().map(|k| k.canonical_phrase()).collect();
    format!(
        "⚠️  delib-prep lint: detected stress-test keyword(s) [{}] in --var question \
         without --class stress-test. Per ADR-085, this deliberation may need a \
         pre-commitment seal. Either pass `--class stress-test` to opt into the \
         seal, or rephrase the question to remove the keyword.",
        phrases.join(", ")
    )
}

/// Run the M5 lint and emit an `eprintln!` warning when keywords are
/// detected and the molecule class is not already `StressTest`. No-op
/// when the formula is not `deep-think`, when no `question` variable is
/// provided, or when the operator has correctly declared the class.
///
/// Warning-not-error by design: the operator may legitimately discuss
/// these phrases without invoking the seal mechanism (e.g. a tactical
/// post-mortem of a past stress-test). The lint surfaces the mismatch;
/// it does not refuse dispatch.
pub fn lint_deep_think(
    formula_name: &str,
    question: Option<&str>,
    class: MoleculeClass,
) -> Option<String> {
    if formula_name != "deep-think" {
        return None;
    }
    if class == MoleculeClass::StressTest {
        return None;
    }
    let question = question?;
    let matched = detect_keywords(question);
    if matched.is_empty() {
        return None;
    }
    Some(format_warning(&matched))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_canonical_phrases_are_lowercase_kebab_or_space() {
        for kw in StressTestKeyword::ALL {
            let p = kw.canonical_phrase();
            assert_eq!(p, p.to_ascii_lowercase(), "phrase must be lowercase: {p}");
            assert!(!p.is_empty());
        }
    }

    #[test]
    fn detects_each_canonical_phrase() {
        for kw in StressTestKeyword::ALL {
            let text = format!("the panel will run a {} of A1.", kw.canonical_phrase());
            let matched = detect_keywords(&text);
            assert!(
                matched.contains(&kw),
                "expected {kw:?} in {text:?}, got {matched:?}"
            );
        }
    }

    #[test]
    fn detection_is_case_insensitive() {
        let text = "We need a Stress-Test of the Meta-Frame and a Disconfirming Observation.";
        let matched = detect_keywords(text);
        assert!(matched.contains(&StressTestKeyword::StressTest));
        assert!(matched.contains(&StressTestKeyword::MetaFrame));
        assert!(matched.contains(&StressTestKeyword::DisconfirmingObservation));
    }

    #[test]
    fn empty_text_matches_nothing() {
        assert!(detect_keywords("").is_empty());
        assert!(detect_keywords("a tactical question with no markers").is_empty());
    }

    #[test]
    fn lint_skips_non_deep_think_formulas() {
        assert!(lint_deep_think(
            "task-work",
            Some("we need a stress-test"),
            MoleculeClass::Standard,
        )
        .is_none());
    }

    #[test]
    fn lint_skips_when_class_is_stress_test() {
        assert!(lint_deep_think(
            "deep-think",
            Some("we need a stress-test of A1"),
            MoleculeClass::StressTest,
        )
        .is_none());
    }

    #[test]
    fn lint_skips_when_no_question() {
        assert!(lint_deep_think("deep-think", None, MoleculeClass::Standard).is_none());
    }

    #[test]
    fn lint_silent_when_no_keywords() {
        assert!(lint_deep_think(
            "deep-think",
            Some("which architecture should we pick?"),
            MoleculeClass::Standard,
        )
        .is_none());
    }

    #[test]
    fn lint_warns_on_keyword_without_class() {
        let warn = lint_deep_think(
            "deep-think",
            Some("run a stress-test on the meta-frame to find a disconfirming observation"),
            MoleculeClass::Standard,
        )
        .expect("expected warning");
        assert!(warn.contains("stress-test"));
        assert!(warn.contains("meta-frame"));
        assert!(warn.contains("disconfirming observation"));
        assert!(warn.contains("--class stress-test"));
        assert!(warn.contains("ADR-085"));
    }

    #[test]
    fn lint_dedupe_and_order_canonical() {
        let warn = lint_deep_think(
            "deep-think",
            Some("falsification, falsification, pre-commitment, stress-test"),
            MoleculeClass::Standard,
        )
        .expect("expected warning");
        let st = warn.find("stress-test").unwrap();
        let fal = warn.find("falsification").unwrap();
        let pc = warn.find("pre-commitment").unwrap();
        assert!(st < fal && fal < pc, "canonical order: {warn}");
    }

    #[test]
    fn infra_class_is_treated_like_standard() {
        // Only StressTest suppresses the lint — Infra and Standard alike must warn.
        let warn = lint_deep_think("deep-think", Some("a stress-test"), MoleculeClass::Infra);
        assert!(warn.is_some());
    }
}
