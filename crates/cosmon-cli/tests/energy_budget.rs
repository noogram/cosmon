// SPDX-License-Identifier: AGPL-3.0-only

//! Per-molecule energy circuit-breaker tests.
//!
//! Covers the runaway-loop protection named in THESIS Part XI: every
//! `cs evolve` decrements the molecule's [`StepBudget`]. At zero, the next
//! attempt transitions the molecule to `Frozen` with reason
//! `"energy-exhausted"` and refuses to advance — never silent retry.
//!
//! [`StepBudget`]: cosmon_core::energy::StepBudget

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// Helper that builds `cs` with cwd set to a non-git directory so the
/// per-step auto-commit run by `cs evolve` cannot reach into the test
/// harness's own git repo and commit unrelated state.
fn cosmon_bin_in(cwd: &std::path::Path) -> Command {
    let mut cmd = cosmon_bin();
    cmd.current_dir(cwd);
    cmd
}

/// Move a freshly-nucleated molecule into the `running` state by editing its
/// `state.json` directly. Mirrors the helper in `cli.rs` — the typestate lift
/// makes Pending → Running the only legal path, and `cs tackle` (the production
/// route) drags in tmux + worktree creation we don't want in a unit test.
fn mark_molecule_running(state_dir: &std::path::Path, id: &str) {
    let path = state_dir
        .join("fleets/default/molecules")
        .join(id)
        .join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    state["status"] = serde_json::json!("running");
    fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
}

/// Read a molecule's persisted JSON state.
fn read_state(state_dir: &std::path::Path, id: &str) -> serde_json::Value {
    let path = state_dir
        .join("fleets/default/molecules")
        .join(id)
        .join("state.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Exit-criterion test for the energy budget:
///
/// A multi-step molecule with `--energy-budget 3` accepts exactly three
/// `cs evolve` advances; the fourth attempt is refused with reason
/// `"energy-exhausted"` and the molecule transitions to `Frozen`.
///
/// This is the structural circuit breaker that makes silent runaway loops
/// (the Perplexity-Personal-Computer "$200 on a single page" failure mode)
/// impossible — the budget exhaustion *is* the signal, never silent retry.
#[test]
#[allow(clippy::too_many_lines)]
fn evolve_refuses_step_past_energy_budget_and_marks_stuck() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    // Six-step formula — large enough that a budget of 3 hits the breaker
    // long before the molecule completes naturally. No verification gates,
    // no pre-conditions; each step is a no-op the worker "completes" by
    // calling `cs evolve --evidence`.
    let formula_toml = r#"
formula = "energy-budget-test"
version = 1
description = "Six-step formula for the energy circuit breaker test"
id_prefix = "eb"

[[steps]]
id = "step-1"
title = "Step 1"
description = "First step."
acceptance = "Done"

[[steps]]
id = "step-2"
title = "Step 2"
description = "Second step."
acceptance = "Done"
needs = ["step-1"]

[[steps]]
id = "step-3"
title = "Step 3"
description = "Third step."
acceptance = "Done"
needs = ["step-2"]

[[steps]]
id = "step-4"
title = "Step 4"
description = "Fourth step (would run if budget allowed)."
acceptance = "Done"
needs = ["step-3"]

[[steps]]
id = "step-5"
title = "Step 5"
description = "Fifth step."
acceptance = "Done"
needs = ["step-4"]

[[steps]]
id = "step-6"
title = "Step 6"
description = "Sixth step."
acceptance = "Done"
needs = ["step-5"]
"#;
    let formula_path = formulas_dir.join("energy-budget-test.formula.toml");
    fs::write(&formula_path, formula_toml).unwrap();

    let state_str = state_dir.to_str().unwrap();

    // Nucleate with --energy-budget 3. Run with cwd inside the tempdir
    // so the per-step auto-commit baked into `cs evolve` doesn't reach
    // into the test harness's own worktree (the regression cleaned up
    // in 89993865e was operator-visible commits like
    // `evolve(eb-…): step 1/6 — Step 1` showing up on the dev branch).
    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "energy-budget-test",
            "--energy-budget",
            "3",
            "--store-dir",
            state_str,
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();

    // Confirm the budget landed in state.json.
    let initial = read_state(&state_dir, &molecule_id);
    assert_eq!(
        initial["energy_budget"]["cap"], 3,
        "cap should be the value passed to --energy-budget"
    );
    assert_eq!(
        initial["energy_budget"]["remaining"], 3,
        "remaining should equal cap at nucleation"
    );

    mark_molecule_running(&state_dir, &molecule_id);

    // Helper: invoke `cs evolve` and return (success, stdout, stderr).
    let evolve_once = |evidence: &str| -> (bool, String, String) {
        let out = cosmon_bin_in(tmp.path())
            .args([
                "--json",
                "evolve",
                &molecule_id,
                "--evidence",
                evidence,
                "--ops-dir",
                state_str,
                "--formula",
                formula_path.to_str().unwrap(),
            ])
            .output()
            .expect("evolve invocation failed to spawn");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // Three legal evolves — budget decrements 3 → 2 → 1 → 0.
    for n in 1..=3u32 {
        let (ok, _stdout, stderr) = evolve_once(&format!("evidence for step {n}"));
        assert!(ok, "evolve #{n} should succeed: {stderr}");
        let state = read_state(&state_dir, &molecule_id);
        let expected_remaining = 3 - n;
        assert_eq!(
            state["energy_budget"]["remaining"],
            serde_json::json!(expected_remaining),
            "after evolve #{n}: remaining should be {expected_remaining}, got {state}"
        );
        assert_eq!(state["energy_budget"]["cap"], 3, "cap is immutable");
        assert_eq!(
            state["status"], "running",
            "molecule should still be running after evolve #{n}"
        );
    }

    // Fourth attempt — circuit breaker fires.
    let (ok, stdout, stderr) = evolve_once("one-step-too-far");
    assert!(
        !ok,
        "evolve #4 must fail (energy exhausted). stdout={stdout} stderr={stderr}"
    );
    let stuck_payload: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stuck payload should be JSON");
    assert_eq!(
        stuck_payload["energy_exhausted"], true,
        "stuck payload should flag energy_exhausted=true"
    );
    assert_eq!(
        stuck_payload["stuck"], true,
        "stuck payload should flag stuck=true"
    );
    assert_eq!(
        stuck_payload["energy_cap"], 3,
        "stuck payload should echo the immutable cap"
    );

    // Persisted state: molecule is now Frozen, current_step did not advance,
    // budget stays at 0 (never wraps below).
    let final_state = read_state(&state_dir, &molecule_id);
    assert_eq!(
        final_state["status"], "frozen",
        "molecule should be Frozen (cosmon's stuck representation): {final_state}"
    );
    assert_eq!(
        final_state["current_step"], 3,
        "current_step should NOT have advanced past the third successful evolve"
    );
    assert_eq!(
        final_state["energy_budget"]["remaining"], 0,
        "remaining stays at 0 (saturating, never negative)"
    );

    // Event log carries the structured `energy_exhausted` reason so
    // `cs peek` and downstream tooling can categorise the stuck state
    // without parsing the free-form reason string.
    let events_path = state_dir.join("events.jsonl");
    let events = fs::read_to_string(&events_path).expect("events.jsonl should exist");
    assert!(
        events.lines().any(|l| {
            l.contains("\"type\":\"molecule_stuck\"")
                && l.contains("\"reason\":\"energy_exhausted\"")
        }),
        "events.jsonl should contain a molecule_stuck event with reason=energy_exhausted: {events}"
    );
}

/// `--energy-budget 0` disables the breaker for that molecule — `cs evolve`
/// should not stamp a `StepBudget` and never park the molecule for budget
/// reasons. Lets long-running formulas opt out of the cap explicitly.
#[test]
fn nucleate_with_zero_budget_disables_circuit_breaker() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "no-budget-test"
version = 1
description = "Single-step formula"
id_prefix = "nb"

[[steps]]
id = "only"
title = "Only step"
description = "Done."
acceptance = "Done"
"#;
    fs::write(
        formulas_dir.join("no-budget-test.formula.toml"),
        formula_toml,
    )
    .unwrap();

    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "no-budget-test",
            "--energy-budget",
            "0",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();

    let state = read_state(&state_dir, &molecule_id);
    assert!(
        state["energy_budget"].is_null() || state.get("energy_budget").is_none(),
        "--energy-budget 0 must NOT stamp a budget on the molecule, got {state}"
    );
}
