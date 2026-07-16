// SPDX-License-Identifier: AGPL-3.0-only

//! Spec-as-test guard for the `deep-think` formula's opt-in decomposition.
//!
//! task-20260622-29e3 (cosmon-ward from grace) fixed a duplicate-children
//! pathology: the `outcomes` step (step 4) used to instruct the worker to
//! auto-nucleate the recommended child molecules unconditionally. When the
//! pilot — reading the same recommendation — also nucleated those children
//! by hand, every child existed twice, leaving orphan pending DUPLICATES the
//! operator had to collapse one by one (observed on grace: 6 duplicates of
//! C1–C4 + IA-Enfant1/2).
//!
//! The fix makes auto-nucleation OPT-IN via the `decompose` variable:
//! `recommend` (default) writes the proposed decomposition into outcomes.md
//! and lets the pilot own nucleation; `auto` restores the per-child
//! `cosmon_nucleate` path. These assertions fail loudly if the formula drifts
//! back to unconditional auto-nucleation.

use cosmon_core::formula::Formula;

const DEEP_THINK_TOML: &str = include_str!("../../../.cosmon/formulas/deep-think.formula.toml");

#[test]
fn formula_parses() {
    let formula = Formula::parse(DEEP_THINK_TOML).expect("deep-think formula must parse");
    assert_eq!(formula.name.as_str(), "deep-think");
    assert_eq!(formula.id_prefix, "delib");
}

#[test]
fn decompose_variable_is_declared_and_defaults_to_recommend() {
    let formula = Formula::parse(DEEP_THINK_TOML).expect("parse");
    let decompose = formula
        .variables
        .get("decompose")
        .expect("deep-think must declare a `decompose` variable to gate auto-nucleation");
    assert_eq!(
        decompose.default.as_deref(),
        Some("recommend"),
        "the safe default is recommendation-only — auto-nucleation must be opt-in \
         (task-20260622-29e3 grace duplicate-children pathology)"
    );
    assert!(
        !decompose.required,
        "`decompose` must be optional so legacy callers get the safe default"
    );
}

#[test]
fn outcomes_step_documents_optin_auto_nucleation() {
    let formula = Formula::parse(DEEP_THINK_TOML).expect("parse");
    let outcomes = formula
        .steps
        .iter()
        .find(|s| s.id == "outcomes")
        .expect("deep-think must keep an `outcomes` step");
    let desc = outcomes.description.to_lowercase();

    // The step must declare that auto-nucleation is opt-in, name the
    // variable, and name the default mode.
    assert!(
        desc.contains("opt-in") || desc.contains("opt in"),
        "outcomes step must state that auto-nucleation is opt-in"
    );
    assert!(
        desc.contains("decompose=recommend") && desc.contains("decompose=auto"),
        "outcomes step must document both decompose modes by name"
    );
    // The default path must NOT call cosmon_nucleate — it writes outcomes.md.
    assert!(
        desc.contains("does not") && desc.contains("cosmon_nucleate"),
        "outcomes step must state the default does NOT call cosmon_nucleate"
    );
}

#[test]
fn outcomes_step_carries_duplicate_children_regression_guard() {
    let formula = Formula::parse(DEEP_THINK_TOML).expect("parse");
    let outcomes = formula
        .steps
        .iter()
        .find(|s| s.id == "outcomes")
        .expect("outcomes step");
    let desc = outcomes.description.to_lowercase();
    assert!(
        desc.contains("duplicate"),
        "the regression guard for the grace duplicate-children pathology must \
         remain in the outcomes step so the rationale is not silently dropped"
    );
}
