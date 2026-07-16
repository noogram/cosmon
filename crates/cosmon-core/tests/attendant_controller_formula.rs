// SPDX-License-Identifier: AGPL-3.0-only

//! Parser-and-shape test for the `attendant-controller` formula.
//!
//! The attendant-controller is built as a TOML formula composing existing
//! primitives (no new crate, no bash). The formula carries load-bearing
//! discipline that must not be silently refactored away:
//!
//! * **Three steps** in order — `scan` → `policy` → `report`. Skipping
//!   `policy` would turn the controller into a one-shot scanner; merging
//!   `report` into `policy` would lose the time-series invariant on
//!   `attendant-pressure.md`.
//! * **Naming** — `attendant`, never `drain`. wheeler's renaming is
//!   load-bearing: an attendant *attends to* what it cannot yet drain;
//!   a drain removes without learning. Any future drift back to "drain"
//!   in the live verb (description body) breaks this assertion.
//! * **Causal-filter discipline** — the description must mention the
//!   `Attendant` emitter exclusion in the candidate query. Without it
//!   the failure mode is dilution (auto-immune fatigue, hawking §1).
//! * **EMIT > ACT** — default-off discipline. The formula must surface
//!   refusals and `policy_misses`, not silently mutate state.
//!
//! These are spec-as-test assertions: the test fails loudly if the
//! formula drifts away from the discipline articulated above.

use cosmon_core::formula::Formula;

const ATTENDANT_TOML: &str =
    include_str!("../../../.cosmon/formulas/attendant-controller.formula.toml");

#[test]
fn formula_parses_with_three_step_shape() {
    let formula = Formula::parse(ATTENDANT_TOML).expect("attendant-controller formula must parse");
    assert_eq!(formula.name.as_str(), "attendant-controller");
    assert_eq!(formula.id_prefix, "attend");
    assert!(
        !formula.freeze_on_last_step,
        "attendant is a tick, not a persistent actor — must not freeze"
    );

    let step_ids: Vec<_> = formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        step_ids,
        vec!["scan", "policy", "report"],
        "step shape is load-bearing — scan classifies, policy emits \
         suggestions, report appends the time-series tick row"
    );
}

#[test]
fn formula_uses_attendant_naming_not_drain() {
    let formula = Formula::parse(ATTENDANT_TOML).expect("parse");

    let lowered = formula.description.to_lowercase();
    assert!(
        lowered.contains("attendant"),
        "description must use the attendant vocabulary"
    );

    // The historical name "drain" may surface in retrospective notes
    // that EXPLAIN the rename — that is allowed. What is forbidden is
    // the verb-form `drain.<event>` resurfacing in the event schema.
    // The schema lives verbatim in the description; check no event
    // record namespaced as `drain.*` is documented.
    assert!(
        !lowered.contains("drain.candidate")
            && !lowered.contains("drain.acted")
            && !lowered.contains("drain.refused")
            && !lowered.contains("drain.deferred")
            && !lowered.contains("drain.policy_miss")
            && !lowered.contains("drain.tick")
            && !lowered.contains("drain.boundary"),
        "event verbs must be `attendant.*`, never `drain.*` (wheeler \
         §naming, delib-20260509-18df)"
    );
}

#[test]
fn formula_documents_causal_filter_guard() {
    let formula = Formula::parse(ATTENDANT_TOML).expect("parse");
    let lowered = formula.description.to_lowercase();
    assert!(
        lowered.contains("causal-filter") || lowered.contains("causal filter"),
        "the formula must declare its causal-filter guard — without \
         it the failure mode is dilution (hawking §1, \
         delib-20260509-18df §F1)"
    );
    assert!(
        lowered.contains("emitter_kind"),
        "the causal filter is keyed on `emitter_kind` (set by \
         task-20260509-7210); the formula must reference the field \
         it depends on"
    );
}

#[test]
fn formula_documents_emit_greater_than_act_discipline() {
    let formula = Formula::parse(ATTENDANT_TOML).expect("parse");
    let lowered = formula.description.to_lowercase();
    assert!(
        lowered.contains("emit > act")
            || lowered.contains("emit_greater_than_act")
            || lowered.contains("emit more than"),
        "the formula must declare the EMIT > ACT discipline — it is \
         the entire point of v0 (default-off `--apply`)"
    );
    // A `policy_miss` event verb is the IFBDD primitive — without it
    // the formula silently skips unknown (kind, state) tuples and the
    // policy table cannot grow by misses.
    assert!(
        lowered.contains("policy_miss"),
        "policy_miss is the IFBDD primitive — the table grows by \
         misses, not by anticipation; must be in the documented \
         event schema"
    );
}

#[test]
fn formula_emits_full_event_vocabulary() {
    let formula = Formula::parse(ATTENDANT_TOML).expect("parse");
    let lowered = formula.description.to_lowercase();
    // wheeler §A — the seven event verbs of the attendant.
    for verb in [
        "attendant.candidate",
        "attendant.acted",
        "attendant.refused",
        "attendant.deferred",
        "attendant.policy_miss",
        "attendant.tick",
        "attendant.boundary",
    ] {
        assert!(
            lowered.contains(verb),
            "event verb `{verb}` missing from formula description \
             (wheeler §A vocabulary is load-bearing)"
        );
    }
}

#[test]
fn formula_is_tier_zero_no_self_nucleation() {
    let formula = Formula::parse(ATTENDANT_TOML).expect("parse");
    // Tier 0 is the discipline: the attendant observes + emits + may
    // (with --apply) call existing primitives, but it never nucleates
    // children of its own kind. A Tier 1 attendant would risk a
    // self-spawning loop the causal filter cannot fully prevent.
    assert_eq!(
        formula.tier.level(),
        0,
        "attendant must be Tier 0 — observes + emits, does not \
         nucleate (delib-20260509-18df §F1, hawking)"
    );
}

#[test]
fn step_dependencies_chain_linearly() {
    let formula = Formula::parse(ATTENDANT_TOML).expect("parse");
    // scan: no deps; policy: needs scan; report: needs policy.
    // A diamond or a swap would break the artifact-chain invariant
    // (suggestions.md derives from scan-report.md; tick row derives
    // from suggestions.md).
    let scan = formula.steps.iter().find(|s| s.id == "scan").expect("scan");
    let policy = formula
        .steps
        .iter()
        .find(|s| s.id == "policy")
        .expect("policy");
    let report = formula
        .steps
        .iter()
        .find(|s| s.id == "report")
        .expect("report");
    assert!(scan.depends_on.is_empty(), "scan has no predecessors");
    assert_eq!(
        policy
            .depends_on
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["scan"]
    );
    assert_eq!(
        report
            .depends_on
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["policy"]
    );
}
