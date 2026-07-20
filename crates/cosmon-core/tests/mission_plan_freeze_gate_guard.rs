// SPDX-License-Identifier: AGPL-3.0-only

//! Regression guard for the mission-plan / mission-controller auto-freeze gate.
//!
//! ## The bug this guards against
//!
//! An early version of the `mission-plan` (and `mission-controller`) formula
//! ended with a mechanical shell-gate step:
//!
//! ```toml
//! [[steps]]
//! id = "auto-freeze"
//! command = "cs freeze $(cs observe --json | jq -r .id) --reason '...'"
//! ```
//!
//! That gate carried two latent footguns that fired together on
//! `mission-20260530-b4d2` (reproduced in the `project_x` galaxy, migrated to
//! cosmon as `task-20260625-ae13`):
//!
//! 1. **`cs observe --json` with NO molecule id returns a JSON *array*** of
//!    every molecule. Piping that array into `jq -r .id` fails with
//!    `Cannot index array with string "id"`. The command substitution
//!    `$(...)` therefore expanded to an empty/garbage string.
//! 2. **`cs freeze` then ran without its mandatory WORKER positional**, so
//!    the gate exited non-zero — which collapsed the mission molecule even
//!    though the three real cognitive steps (analyze / decompose / verify)
//!    had all succeeded and the child DAG was already posted.
//!
//! ## The cosmon-side correction (already landed, locked here)
//!
//! Two structural mechanisms removed the gate entirely; this test asserts
//! they remain in force so the footgun cannot be re-introduced (including by
//! a stale formula being copied back in from a downstream galaxy):
//!
//! * **`freeze_on_last_step = true`** — the runtime flips the molecule to
//!   `Frozen` when `cs evolve` lands the final step. No shell, no `jq`, no
//!   command substitution, no env-var fragility. The `verify` step is the
//!   last step and the freeze is mechanical *inside the runtime*.
//! * **Typed `[steps.query]`** (see `query-demo.formula.toml`) — the
//!   sanctioned replacement for `cs --json observe … | jq -r .id` when a
//!   step genuinely needs the molecule's own id. A Rust dot-path evaluator
//!   runs against `state.json`; there is no shell to mis-handle the array.
//!
//! These are spec-as-test assertions: the test fails loudly if either
//! formula drifts back toward a self-freezing shell gate.

use cosmon_core::formula::Formula;
use std::path::PathBuf;

const MISSION_PLAN_TOML: &str = include_str!("../../../.cosmon/formulas/mission-plan.formula.toml");
const MISSION_CONTROLLER_TOML: &str =
    include_str!("../../../.cosmon/formulas/mission-controller.formula.toml");

/// Resolve the repo's `.cosmon/formulas` directory from this crate's
/// manifest dir, so the federated guard can iterate every formula.
fn formulas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(".cosmon")
        .join("formulas")
}

#[test]
fn mission_plan_freezes_via_runtime_not_shell_gate() {
    let formula = Formula::parse(MISSION_PLAN_TOML).expect("mission-plan must parse");
    assert_eq!(formula.name.as_str(), "mission-plan");
    assert_eq!(formula.id_prefix, "mission");

    // The runtime-enforced freeze is the WHOLE point — without it the
    // formula would need the buggy shell gate back.
    assert!(
        formula.freeze_on_last_step,
        "mission-plan MUST declare freeze_on_last_step = true — the runtime \
         freezes the planner mechanically when `cs evolve` lands the last \
         step. Dropping this flag would resurrect the need for a \
         `cs freeze $(cs observe …)` shell gate (the auto-freeze footgun)."
    );

    // The final step is the cognitive `verify` step, NOT a mechanical
    // `auto-freeze` shell gate.
    let step_ids: Vec<&str> = formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        step_ids,
        vec!["analyze", "decompose", "verify"],
        "mission-plan step shape is load-bearing — the last step must be \
         `verify`, not a re-introduced `auto-freeze` shell gate"
    );
    assert!(
        !step_ids.contains(&"auto-freeze"),
        "the `auto-freeze` shell-gate step must stay deleted — freeze is \
         runtime-enforced via freeze_on_last_step"
    );
}

#[test]
fn mission_controller_freezes_via_runtime_not_shell_gate() {
    let formula = Formula::parse(MISSION_CONTROLLER_TOML).expect("mission-controller must parse");
    assert!(
        formula.freeze_on_last_step,
        "mission-controller MUST declare freeze_on_last_step = true — same \
         runtime-enforced freeze as mission-plan (both formulas received the \
         buggy `auto-freeze` gate in commit 1655c2d6e and both were fixed)"
    );
    let step_ids: Vec<&str> = formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert!(
        !step_ids.contains(&"auto-freeze"),
        "mission-controller must not carry an `auto-freeze` shell-gate step"
    );
}

#[test]
fn mission_planner_formulas_have_no_self_freeze_shell_gate() {
    for (label, toml) in [
        ("mission-plan", MISSION_PLAN_TOML),
        ("mission-controller", MISSION_CONTROLLER_TOML),
    ] {
        let formula = Formula::parse(toml).unwrap_or_else(|e| panic!("{label} must parse: {e}"));
        for step in &formula.steps {
            assert!(
                step.command.is_none(),
                "{label} step `{}` declares a `command` shell gate — the \
                 mission-planner formulas must have NO shell gates at all. \
                 Freeze is runtime-enforced; self-id extraction uses the \
                 typed [steps.query] mechanism, never a `cs observe | jq` \
                 shell-out.",
                step.id
            );
        }
    }
}

/// Federated guard across EVERY formula in `.cosmon/formulas`: no shell-gate
/// `command` may self-freeze (`cs freeze` from inside a gate) or extract the
/// molecule's own id by piping a bare `cs observe` into `jq … .id`.
///
/// This is the durable fence: even if a stale formula is copied back in from
/// a downstream galaxy (the exact provenance of this bug — `project_x` ran a
/// stale `mission-plan` copy), the re-introduced footgun fails CI here rather
/// than silently collapsing a mission molecule at runtime.
#[test]
fn no_formula_command_gate_self_freezes_or_extracts_self_id() {
    let dir = formulas_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read formulas dir {}: {e}", dir.display()));

    let mut checked = 0usize;
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let toml = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        // Some files in the dir may be non-formula fixtures; skip anything
        // that does not parse as a formula rather than failing the guard on
        // unrelated content.
        let Ok(formula) = Formula::parse(&toml) else {
            continue;
        };
        checked += 1;

        for step in &formula.steps {
            let Some(cmd) = step.command.as_deref() else {
                continue;
            };
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

            // Footgun A — self-freeze from inside a shell gate. The runtime's
            // `freeze_on_last_step` is the only sanctioned freeze path for a
            // formula; a `cs freeze` shell gate re-imports the missing-WORKER
            // and array-jq failure modes.
            assert!(
                !cmd.contains("cs freeze"),
                "formula `{name}` step `{}` shell gate calls `cs freeze` — \
                 self-freezing from a shell gate is forbidden. Use \
                 `freeze_on_last_step = true` (runtime-enforced) instead. \
                 This is the auto-freeze footgun (mission-20260530-b4d2).",
                step.id
            );

            // Footgun B — self-id extraction by piping a bare `cs observe`
            // (no molecule id) into jq. Without an id, `cs observe --json`
            // emits a JSON array and `jq -r .id` fails with
            // "Cannot index array with string id". Use the typed
            // [steps.query] mechanism (query-demo.formula.toml) instead.
            let normalized: String = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
            let pipes_observe_to_jq =
                normalized.contains("cs observe") || normalized.contains("cs --json observe");
            assert!(
                !(pipes_observe_to_jq && normalized.contains("jq") && normalized.contains(".id")),
                "formula `{name}` step `{}` shell gate extracts a molecule id \
                 via `cs observe … | jq … .id` — a bare `cs observe` returns \
                 an array and `jq -r .id` fails. Use the typed [steps.query] \
                 step (source = \"state\", expr = \".id\") instead.",
                step.id
            );
        }
    }

    assert!(
        checked >= 10,
        "expected to scan the full formula library (>= 10 formulas), only \
         parsed {checked} — the federated guard is not actually covering the \
         directory ({})",
        dir.display()
    );
}
