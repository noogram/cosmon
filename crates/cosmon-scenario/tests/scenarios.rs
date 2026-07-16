// SPDX-License-Identifier: AGPL-3.0-only

//! Integration: every committed scenario file must pass.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/cosmon-scenario -> repo root
    p.pop();
    p.pop();
    p
}

#[test]
fn all_tests_scenarios_pass() {
    let pattern = repo_root().join("tests/scenarios/*.toml");
    let files = cosmon_scenario::discover(&pattern.to_string_lossy()).expect("discover scenarios");
    assert!(!files.is_empty(), "no scenarios found");
    let mut failed = Vec::new();
    for f in &files {
        let r = cosmon_scenario::run_scenario(f);
        if !r.passed {
            failed.push((r.name.clone(), r.failures.clone()));
        }
    }
    assert!(failed.is_empty(), "scenario failures: {failed:#?}");
}

/// Pins the *pre-fix* behavior of option B: with
/// `decay_collapse_releases = false`, lateral `DecayProduct` children of
/// a `Collapsed` parent stay orphaned in `Pending`. This is the exact
/// pathology `cs run` exhibited before the fix — see
/// `DIAGNOSIS-mission-collapse.md`. The committed scenario file
/// `collapsed-mission-orphans-children.toml` pins the *post-fix* state.
#[test]
fn collapsed_mission_orphans_children_pre_fix_is_red() {
    let path = repo_root().join("tests/scenarios/collapsed-mission-orphans-children.toml");
    let scenario = cosmon_scenario::load_scenario(&path).expect("load scenario");
    let mut eng = cosmon_scenario::Engine::new(&scenario).expect("engine");
    eng.set_decay_collapse_releases(false);
    eng.run(&scenario.actions).expect("run");
    for id in ["C1", "C2"] {
        let mol = eng.mols.get(id).expect("child present");
        assert_eq!(
            mol.status,
            cosmon_scenario::Status::Pending,
            "pre-fix: {id} must be orphaned pending",
        );
    }
    // And the post-fix assertions must fail under the pre-fix setting.
    let mut any_failed = false;
    for a in &scenario.asserts {
        if eng.check(a).is_err() {
            any_failed = true;
            break;
        }
    }
    assert!(
        any_failed,
        "pre-fix engine must violate at least one post-fix assertion"
    );
}

#[test]
fn inline_merge_before_dispatch() {
    let toml_src = r#"
[scenario]
name = "inline"
[[given.molecules]]
id = "A"
steps = [ {name="s1", native="cosmon::test::noop"} ]
[[given.molecules]]
id = "B"
steps = [ {name="s1", native="cosmon::test::noop"} ]
[[given.links]]
from = "A"
to = "B"
kind = "Blocks"
[[actions]]
op = "run_root"
target = "B"
[[assert]]
molecule = "B"
status = "completed"
[[assert]]
property = "merge_before_dispatch"
"#;
    let s: cosmon_scenario::Scenario = toml::from_str(toml_src).unwrap();
    let mut eng = cosmon_scenario::Engine::new(&s).unwrap();
    eng.run(&s.actions).unwrap();
    for a in &s.asserts {
        eng.check(a).expect("assertion");
    }
}
