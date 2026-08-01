// SPDX-License-Identifier: AGPL-3.0-only

//! An unresolvable `formula_id` must be *named*, not silently degraded
//! (task-20260725-eb3b).
//!
//! A molecule carries its formula by **id**; `cs tackle` resolves that id
//! against the mission project's `.cosmon/formulas/`. A polymer germinated
//! from a spore whose recipes were never installed into that registry
//! therefore loses every per-step `adapter` / `model` pin it declares — and
//! before this fix the run said nothing: the `model_selected` event recorded
//! `source = default` with a `fallback_reason` reading "no formula-step model
//! pin", which describes a formula that pins nothing, not one that was never
//! found. Observed twice on real full-lane spore runs; on one of them 23 nodes
//! ran flat on the adapter default while the shipped tiering was inert.
//!
//! These tests pin the two halves of the fix: the warning on stderr, and the
//! recorded reason in the event an audit reads afterwards.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // Hermetic resolution chain: strip the operator's session hammers so
        // the test exercises the documented order, and point the machine-wide
        // config tier at an empty dir under the per-test tmp.
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env_remove("COSMON_DEFAULT_MODEL")
        .env_remove("ANTHROPIC_MODEL")
        .env_remove("COSMON_FORMULAS_DIR")
        .env("COSMON_CONFIG_HOME", cwd.join("isolated-config-home"));
    cmd
}

/// A one-step formula pinning a model — the tiering a spore ships.
const PINNED_FORMULA: &str = r#"
formula = "unresolvable-pin-test"
version = 1
description = "One-step formula whose step pins a model"
id_prefix = "upt"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step pinning a model."
acceptance = "Done"
model = "claude-opus-5"
"#;

/// Germinate a molecule from a formula that lives *outside* the mission
/// registry — the spore-bundle situation — and return
/// `(tmp, state_dir, molecule_id)`.
///
/// `cs nucleate --formulas-dir <bundle>` accepts the recipe by path, exactly
/// as `cs spore run` does; the molecule then stores only the id. Nothing is
/// ever written into `<tmp>/.cosmon/formulas/`, so `cs tackle`'s walk-up
/// resolution finds nothing — which is the whole point.
fn setup_molecule_with_unregistered_formula() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let bundle_dir = tmp.path().join("bundle");
    fs::create_dir_all(&bundle_dir).unwrap();
    fs::write(
        bundle_dir.join("unresolvable-pin-test.formula.toml"),
        PINNED_FORMULA,
    )
    .unwrap();

    // The mission project: a config, and a formulas registry that does NOT
    // hold the bundle's recipe.
    let cosmon_dir = tmp.path().join(".cosmon");
    fs::create_dir_all(cosmon_dir.join("formulas")).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"unresolvable-formula-test-eb3b\"\n",
    )
    .unwrap();

    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "unresolvable-pin-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            bundle_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed to spawn");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();

    (tmp, state_dir, molecule_id)
}

/// Every envelope of `type` in `<state_dir>/events.jsonl`.
fn events_of_type(state_dir: &Path, ty: &str) -> Vec<serde_json::Value> {
    let raw = fs::read_to_string(state_dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("type").and_then(serde_json::Value::as_str) == Some(ty))
        .collect()
}

fn tackle_dry_run(tmp: &Path, state_dir: &Path, mol_id: &str) -> std::process::Output {
    let output = cosmon_bin_in(tmp)
        .args([
            "tackle",
            mol_id,
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed to spawn");
    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// The operator is told the id did not resolve, and where cosmon looked —
/// the remedy is to install the recipe *there*, so the path is the message.
#[test]
fn unresolvable_formula_id_warns_on_stderr() {
    let (tmp, state_dir, mol_id) = setup_molecule_with_unregistered_formula();
    let output = tackle_dry_run(tmp.path(), &state_dir, &mol_id);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("formula `unresolvable-pin-test` not found in the registry"),
        "stderr must name the unresolved formula id, got:\n{stderr}"
    );
    assert!(
        stderr.contains("unresolvable-pin-test.formula.toml"),
        "stderr must name the path that was searched, got:\n{stderr}"
    );
}

/// The recorded reason must not claim "no formula-step model pin" when the
/// truth is that the formula was never found. This is the half an audit
/// reads: the published run attributed work to a model allocation that never
/// happened, and the event was the only place that could have said so.
#[test]
fn unresolvable_formula_id_is_named_in_the_model_selected_event() {
    let (tmp, state_dir, mol_id) = setup_molecule_with_unregistered_formula();
    tackle_dry_run(tmp.path(), &state_dir, &mol_id);

    let events = events_of_type(&state_dir, "model_selected");
    assert_eq!(
        events.len(),
        1,
        "expected one model_selected event: {events:?}"
    );
    let source = events[0]
        .get("selection_source")
        .expect("selection_source on model_selected");
    assert_eq!(source["source"], "default", "source: {source}");
    let reason = source["fallback_reason"].as_str().expect("fallback_reason");
    assert!(
        reason.contains("not found in the registry"),
        "fallback_reason must name the unresolved reference, got: {reason}"
    );
    assert!(
        reason.contains("unresolvable-pin-test"),
        "fallback_reason must name the formula id, got: {reason}"
    );
}

/// The same sharpening on the adapter axis — an unresolvable id costs the
/// step's `adapter` pin exactly as it costs its `model` pin.
#[test]
fn unresolvable_formula_id_is_named_in_the_adapter_selected_event() {
    let (tmp, state_dir, mol_id) = setup_molecule_with_unregistered_formula();
    tackle_dry_run(tmp.path(), &state_dir, &mol_id);

    let events = events_of_type(&state_dir, "adapter_selected");
    assert_eq!(
        events.len(),
        1,
        "expected one adapter_selected event: {events:?}"
    );
    let source = events[0]
        .get("selection_source")
        .expect("selection_source on adapter_selected");
    assert_eq!(source["source"], "default", "source: {source}");
    let reason = source["fallback_reason"].as_str().expect("fallback_reason");
    assert!(
        reason.contains("not found in the registry"),
        "fallback_reason must name the unresolved reference, got: {reason}"
    );
}

/// The converse, and the reason this is a *sharpening* and not a blanket
/// warning: a formula that resolves and simply declares no pin is a
/// deliberate absence. Its reason must stay the old one, and no warning may
/// be printed — otherwise the new sentence becomes noise and stops being read.
#[test]
fn resolvable_formula_without_pins_keeps_the_plain_no_pin_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let cosmon_dir = tmp.path().join(".cosmon");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"unresolvable-formula-test-eb3b\"\n",
    )
    .unwrap();
    let unpinned = r#"
formula = "registered-unpinned-test"
version = 1
description = "One-step formula that deliberately pins nothing"
id_prefix = "rut"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step, no pins."
acceptance = "Done"
"#;
    fs::write(
        formulas_dir.join("registered-unpinned-test.formula.toml"),
        unpinned,
    )
    .unwrap();

    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "registered-unpinned-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed to spawn");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mol_id =
        serde_json::from_str::<serde_json::Value>(String::from_utf8_lossy(&output.stdout).trim())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();

    let output = tackle_dry_run(tmp.path(), &state_dir, &mol_id);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("not found in the registry"),
        "a resolvable formula must not warn, got:\n{stderr}"
    );

    let events = events_of_type(&state_dir, "model_selected");
    assert_eq!(events.len(), 1, "expected one model_selected event");
    let reason = events[0]["selection_source"]["fallback_reason"]
        .as_str()
        .expect("fallback_reason");
    assert!(
        !reason.contains("not found in the registry"),
        "a deliberate absence must keep the plain no-pin reason, got: {reason}"
    );
    assert!(
        reason.contains("no formula-step model pin"),
        "expected the plain no-pin reason, got: {reason}"
    );
}
