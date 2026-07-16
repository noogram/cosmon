// SPDX-License-Identifier: AGPL-3.0-only

//! Regression guard for the producer-work formula's functional gate.
//!
//! A generic compile/test workflow can prove only that code is well-formed.
//! Producers additionally need one output from their actual dispatch path.

use cosmon_core::formula::Formula;

const PRODUCER_WORK_TOML: &str =
    include_str!("../../../.cosmon/formulas/producer-work.formula.toml");
const TASK_WORK_TOML: &str = include_str!("../../../.cosmon/formulas/task-work.formula.toml");

#[test]
fn producer_work_executes_dispatch_before_verify_and_requires_output() {
    let formula = Formula::parse(PRODUCER_WORK_TOML).expect("producer-work must parse");
    let ids: Vec<_> = formula.steps.iter().map(|step| step.id.as_str()).collect();
    assert_eq!(ids, ["implement", "smoke-dispatch", "verify"]);

    let smoke = formula
        .steps
        .iter()
        .find(|step| step.id == "smoke-dispatch")
        .expect("producer-work has smoke-dispatch");
    assert_eq!(
        smoke.command.as_deref(),
        Some("test -x ./smoke-dispatch.sh && ./smoke-dispatch.sh")
    );
    assert_eq!(smoke.expected_artifacts, ["dispatch-output/"]);

    let verify = formula
        .steps
        .iter()
        .find(|step| step.id == "verify")
        .expect("producer-work has verify");
    assert_eq!(verify.depends_on, ["smoke-dispatch"]);
}

#[test]
fn task_work_routes_producers_to_the_dispatch_proof_formula() {
    let task_work = Formula::parse(TASK_WORK_TOML).expect("task-work must parse");
    assert!(task_work.description.contains("producer-work"));
    assert!(task_work.description.contains("smoke-dispatch"));
}
