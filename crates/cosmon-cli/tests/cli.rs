// SPDX-License-Identifier: AGPL-3.0-only

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // These fixtures exercise non-trust behaviour in throwaway repos; the
        // repo-supplied-shell trust gate (B5) is bypassed here so the
        // verification/gate paths run. The gate itself is covered by
        // `src/trust.rs` unit tests and `tests/trust_gate_cli.rs`.
        .env("COSMON_ASSUME_TRUSTED", "1");
    cmd
}

#[test]
fn test_cli_help_renders() {
    let output = cosmon_bin().arg("--help").output().expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("ensemble"));
    assert!(stdout.contains("nucleate"));
    assert!(stdout.contains("observe"));
    assert!(stdout.contains("evolve"));
    assert!(stdout.contains("collapse"));
    assert!(stdout.contains("tackle"));
    assert!(stdout.contains("kill"));
    assert!(stdout.contains("quench"));
    assert!(stdout.contains("prime"));
    assert!(stdout.contains("patrol"));
}

#[test]
fn test_help_subcommand_lists_all_commands() {
    let output = cosmon_bin()
        .arg("help")
        .output()
        .expect("failed to run cs help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "cs help failed: {stdout}");

    // Every registered subcommand (except help itself) must appear
    let expected = [
        "nucleate",
        "observe",
        "evolve",
        "complete",
        "collapse",
        "stuck",
        "freeze",
        "thaw",
        "decay",
        "merge",
        "transform",
        "ensemble",
        "kill",
        "quench",
        "purge",
        "teardown",
        "resume",
        "patrol",
        "fleet",
        "wait",
        "tackle",
        "done",
        "run",
        "init",
        "status",
        "reconcile",
        "migrate",
        "deps",
        "topology",
        "prime",
    ];
    for cmd in &expected {
        assert!(
            stdout.contains(cmd),
            "cs help output missing command: {cmd}"
        );
    }

    // Every verb retired by ADR-052 §CLI delta must be absent
    let retired = [
        "cs touch",
        "cs expire",
        "cs rolling-restart",
        "cs preempt",
        "cs recover",
        "cs spawn",
        "cs dispatch",
        "cs deploy",
        "cs watch",
        "cs creative",
    ];
    for verb in &retired {
        assert!(
            !stdout.contains(verb),
            "cs help still references retired verb: {verb}"
        );
    }

    // Verify grouped headings
    assert!(stdout.contains("Molecule lifecycle:"));
    assert!(stdout.contains("Fleet management:"));
    assert!(stdout.contains("Execution:"));
    assert!(stdout.contains("Project:"));
    assert!(stdout.contains("Tools:"));
}

#[test]
fn test_cli_version() {
    let output = cosmon_bin()
        .arg("--version")
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("cs"));
}

/// `cs init --yes` must be accepted by the parser and run without
/// reading from stdin, so the README quickstart stays paste-testable
/// (knuth invariant). We feed an empty stdin and a strict timeout so a
/// regression that re-introduced a prompt would hang and fail.
#[test]
fn test_init_yes_non_interactive() {
    use std::io::Write;
    use std::process::Stdio;
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("proj");
    fs::create_dir_all(&target).unwrap();

    let mut child = cosmon_bin()
        .args(["init", "--yes", target.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn failed");
    // Close stdin immediately — any prompt would block on read.
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait failed");
    assert!(
        output.status.success(),
        "cs init --yes failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(target.join(".cosmon").exists());

    // Also accept short form `-y`.
    let tmp2 = tempfile::tempdir().unwrap();
    let status = cosmon_bin()
        .args(["init", "-y", tmp2.path().to_str().unwrap()])
        .stdin(Stdio::null())
        .status()
        .expect("run failed");
    assert!(status.success());

    // And document the flag in help.
    let help = cosmon_bin()
        .args(["init", "--help"])
        .output()
        .expect("help failed");
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    let _ = writeln!(std::io::stderr(), "{help_stdout}");
    assert!(help_stdout.contains("--yes"));
}

#[test]
fn test_unknown_command_errors() {
    let output = cosmon_bin()
        .arg("nonexistent")
        .output()
        .expect("failed to run");
    assert!(!output.status.success());
}

#[test]
fn test_json_flag_appears_in_help() {
    let output = cosmon_bin().arg("--help").output().expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--json"));
}

// `cs spawn`, `cs dispatch`, `cs deploy`, `cs watch`, `cs creative`,
// `cs preempt`, `cs recover`, `cs rolling-restart`, `cs touch`, and
// `cs expire` were retired by ADR-052 §CLI delta. The lifecycle tests
// that exercised `spawn + dispatch` were deleted along with them —
// equivalent coverage flows through `cs tackle` in the fleet integration
// suite (see `tests/zombie_prevention.rs`).

/// `cs nucleate --from <dir>` hydrates every declaration in the directory.
///
/// Covers: declaration-provided `id_prefix` override, variables passthrough,
/// worker assignment from the declaration, molecule kind, and links.
#[test]
#[allow(clippy::too_many_lines)]
fn test_nucleate_from_declarations_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    let molecules_dir = tmp.path().join("molecules");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::create_dir_all(&molecules_dir).unwrap();

    // Formula: two-step task. Declaration will override the id_prefix.
    let formula_toml = r#"
formula = "decl-test"
version = 1
description = "Decl hydration test formula"
id_prefix = "fml"

[[steps]]
id = "do"
title = "Do the thing"
description = "Work."
acceptance = "Done"

[[steps]]
id = "check"
title = "Check the thing"
description = "Verify."
needs = ["do"]
"#;
    fs::write(formulas_dir.join("decl-test.formula.toml"), formula_toml).unwrap();

    // Two declarations in the same directory — one kind=task, one kind=idea.
    let decl_a = r#"
id_prefix = "alpha"
formula = "decl-test"
description = "First declaration"
kind = "task"
assign = "worker-a"
links = ["parent-20260407-abcd"]

[variables]
topic = "Alpha molecule"
"#;
    let decl_b = r#"
id_prefix = "beta"
formula = "decl-test"
description = "Second declaration"
kind = "idea"

[variables]
topic = "Beta molecule"
detail = "extra"
"#;
    fs::write(molecules_dir.join("alpha.toml"), decl_a).unwrap();
    fs::write(molecules_dir.join("beta.toml"), decl_b).unwrap();

    let output = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "--from",
            molecules_dir.to_str().unwrap(),
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate --from failed");
    assert!(
        output.status.success(),
        "nucleate --from should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("JSON output invalid: {e}\n{stdout}"));
    let arr = parsed
        .as_array()
        .expect("batch output should be a JSON array");
    assert_eq!(arr.len(), 2, "expected 2 molecules, got: {stdout}");

    // Entries come back in sorted declaration-file order: alpha, beta.
    let alpha_id = arr[0]["id"].as_str().unwrap();
    let beta_id = arr[1]["id"].as_str().unwrap();
    assert!(
        alpha_id.starts_with("alpha-"),
        "id should use declaration's id_prefix, got: {alpha_id}"
    );
    assert!(
        beta_id.starts_with("beta-"),
        "id should use declaration's id_prefix, got: {beta_id}"
    );
    assert_eq!(arr[0]["formula"], "decl-test");
    assert_eq!(arr[0]["total_steps"], 2);
    assert_eq!(arr[0]["assigned_worker"], "worker-a");
    assert!(arr[1]["assigned_worker"].is_null());

    // Persisted state reflects kind + links.
    let alpha_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(alpha_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(alpha_state["kind"], "task");
    assert_eq!(alpha_state["links"][0], "parent-20260407-abcd");
    assert_eq!(alpha_state["variables"]["topic"], "Alpha molecule");
    // Alpha has an assigned worker so status should be queued.
    assert_eq!(alpha_state["status"], "queued");

    let beta_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(beta_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(beta_state["kind"], "idea");
    assert_eq!(beta_state["variables"]["detail"], "extra");
    // Beta has no worker → pending.
    assert_eq!(beta_state["status"], "pending");
}

/// `cs nucleate --blocks` creates a new molecule with a Blocks link and
/// adds the symmetric `BlockedBy` link to the target. The second creation
/// (`cs nucleate --blocked-by`) proves the reverse direction works too.
///
/// Covers ADR-016 Phase 1: symmetric blocking-edge maintenance.
#[test]
#[allow(clippy::too_many_lines)]
fn test_nucleate_blocks_maintains_symmetry() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "blk-test"
version = 1
description = "Blocks/BlockedBy E2E formula"
id_prefix = "blk"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;
    fs::write(formulas_dir.join("blk-test.formula.toml"), formula_toml).unwrap();

    // Create the parent first — it will be blocked by children created after.
    let parent_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "blk-test",
            "--var",
            "topic=parent",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate parent failed");
    assert!(
        parent_out.status.success(),
        "nucleate parent: {}",
        String::from_utf8_lossy(&parent_out.stderr)
    );
    let parent_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&parent_out.stdout).trim()).unwrap();
    let parent_id = parent_json["id"].as_str().unwrap().to_owned();

    // Create a child that blocks the parent. The child's typed_links should
    // contain Blocks{parent}, and the parent should gain BlockedBy{child}.
    let child_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "blk-test",
            "--var",
            "topic=child",
            "--blocks",
            &parent_id,
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate child --blocks failed");
    assert!(
        child_out.status.success(),
        "nucleate child: {}",
        String::from_utf8_lossy(&child_out.stderr)
    );
    let child_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&child_out.stdout).trim()).unwrap();
    let child_id = child_json["id"].as_str().unwrap().to_owned();

    // Read both state files back and verify symmetry.
    let child_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&child_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let child_links = child_state["typed_links"].as_array().unwrap();
    assert_eq!(child_links.len(), 1, "child should have one Blocks link");
    assert_eq!(child_links[0]["rel"], "blocks");
    assert_eq!(child_links[0]["target"], parent_id);

    let parent_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&parent_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let parent_links = parent_state["typed_links"].as_array().unwrap();
    assert_eq!(
        parent_links.len(),
        1,
        "parent should gain symmetric BlockedBy"
    );
    assert_eq!(parent_links[0]["rel"], "blocked_by");
    assert_eq!(parent_links[0]["source"], child_id);

    // Test the reverse flag: nucleate a grandchild with --blocked-by child.
    // Grandchild gets BlockedBy{child}, child gains Blocks{grandchild}.
    let grandchild_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "blk-test",
            "--var",
            "topic=grandchild",
            "--blocked-by",
            &child_id,
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate grandchild --blocked-by failed");
    assert!(grandchild_out.status.success());
    let grandchild_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&grandchild_out.stdout).trim()).unwrap();
    let grandchild_id = grandchild_json["id"].as_str().unwrap().to_owned();

    let grandchild_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&grandchild_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let gc_links = grandchild_state["typed_links"].as_array().unwrap();
    assert_eq!(gc_links.len(), 1);
    assert_eq!(gc_links[0]["rel"], "blocked_by");
    assert_eq!(gc_links[0]["source"], child_id);

    // Child should now have TWO links: Blocks{parent} (from before) AND
    // Blocks{grandchild} (from the symmetric add just now).
    let child_state_v2: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&child_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let child_links_v2 = child_state_v2["typed_links"].as_array().unwrap();
    assert_eq!(
        child_links_v2.len(),
        2,
        "child should have original Blocks + new Blocks after grandchild creation"
    );
    let has_blocks_parent = child_links_v2
        .iter()
        .any(|l| l["rel"] == "blocks" && l["target"] == parent_id);
    let has_blocks_grandchild = child_links_v2
        .iter()
        .any(|l| l["rel"] == "blocks" && l["target"] == grandchild_id);
    assert!(has_blocks_parent, "child should still block parent");
    assert!(has_blocks_grandchild, "child should block grandchild");
}

/// `cs nucleate --blocks <unknown>` must fail fast before persisting the
/// new molecule, otherwise the DAG would contain a dangling reference.
#[test]
fn test_nucleate_blocks_rejects_unknown_target() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "blk-reject"
version = 1
description = "rejection test"
id_prefix = "rej"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;
    fs::write(formulas_dir.join("blk-reject.formula.toml"), formula_toml).unwrap();

    let output = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "blk-reject",
            "--var",
            "topic=orphan",
            "--blocks",
            "task-20260409-ghst",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate should run");
    assert!(
        !output.status.success(),
        "nucleation with dangling --blocks should fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        combined.contains("unknown molecule") || combined.contains("task-20260409-ghst"),
        "error should mention the unknown target, got: {combined}"
    );

    // And the new molecule must not have been persisted.
    let mols_dir = state_dir.join("fleets/default/molecules");
    if mols_dir.exists() {
        let entries: Vec<_> = fs::read_dir(&mols_dir).unwrap().flatten().collect();
        assert!(
            entries.is_empty(),
            "no molecule should be persisted after a rejection"
        );
    }
}

/// Regression guard for the mailroom-20260414-cb10 orphan cascade:
/// a worker that forgets to pass `--blocked-by`/`--decayed-from` must NOT
/// produce orphaned children anymore. The auto-parent contract plumbs a
/// `COSMON_PARENT_MOL_ID` env var from `cs tackle` into every `cs nucleate`
/// the worker issues, synthesizing a `DecayedFrom` edge on the child and a
/// symmetric `DecayProduct` edge on the parent. `--no-parent` and any
/// explicit edge flag must suppress the synthesis so the contract remains
/// opt-out.
#[test]
#[allow(clippy::too_many_lines)]
fn test_nucleate_auto_parent_contract_from_env_var() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "auto-parent"
version = 1
description = "auto-parent env contract"
id_prefix = "ap"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;
    fs::write(formulas_dir.join("auto-parent.formula.toml"), formula_toml).unwrap();

    // Step 1 — create a parent molecule the normal way (no env var set).
    let parent_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "auto-parent",
            "--var",
            "topic=mission",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .env_remove("COSMON_PARENT_MOL_ID")
        .output()
        .expect("nucleate parent failed");
    assert!(
        parent_out.status.success(),
        "parent nucleate: {}",
        String::from_utf8_lossy(&parent_out.stderr)
    );
    let parent_id = serde_json::from_str::<serde_json::Value>(
        String::from_utf8_lossy(&parent_out.stdout).trim(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Step 2 — nucleate 5 children WITHOUT any explicit --blocked-by, but
    // with COSMON_PARENT_MOL_ID set. Each child must auto-link to the parent.
    // This simulates the mailroom worker that forgot the flag 5 times.
    let mut child_ids = Vec::new();
    for i in 0..5 {
        let child_out = cosmon_bin()
            .args([
                "--json",
                "nucleate",
                "auto-parent",
                "--var",
                &format!("topic=child{i}"),
                "--store-dir",
                state_dir.to_str().unwrap(),
                "--formulas-dir",
                formulas_dir.to_str().unwrap(),
            ])
            .env("COSMON_PARENT_MOL_ID", &parent_id)
            .output()
            .expect("nucleate child failed");
        assert!(
            child_out.status.success(),
            "child {i} nucleate: {}",
            String::from_utf8_lossy(&child_out.stderr)
        );
        let stderr = String::from_utf8_lossy(&child_out.stderr);
        assert!(
            stderr.contains("auto-linked to parent"),
            "child {i} should emit auto-link hint on stderr, got: {stderr}"
        );
        let id = serde_json::from_str::<serde_json::Value>(
            String::from_utf8_lossy(&child_out.stdout).trim(),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        child_ids.push(id);
    }

    // Step 3 — every child must carry a DecayedFrom link to the parent.
    for (i, id) in child_ids.iter().enumerate() {
        let state: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(
                state_dir
                    .join("fleets/default/molecules")
                    .join(id)
                    .join("state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let links = state["typed_links"].as_array().unwrap();
        let has_edge = links
            .iter()
            .any(|l| l["rel"] == "decayed_from" && l["id"] == parent_id);
        assert!(
            has_edge,
            "child {i} ({id}) should have DecayedFrom edge to parent, typed_links = {links:?}"
        );
    }

    // Step 4 — the parent must have 5 symmetric DecayProduct edges, one per child.
    let parent_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&parent_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let parent_links = parent_state["typed_links"].as_array().unwrap();
    let decay_products: Vec<&serde_json::Value> = parent_links
        .iter()
        .filter(|l| l["rel"] == "decay_product")
        .collect();
    assert_eq!(
        decay_products.len(),
        5,
        "parent should gain 5 DecayProduct edges (one per child), got {} in {:?}",
        decay_products.len(),
        parent_links
    );
    for id in &child_ids {
        assert!(
            decay_products.iter().any(|l| l["id"] == *id),
            "parent missing DecayProduct pointing at child {id}"
        );
    }

    // Step 5 — --no-parent must suppress the synthesis even when env is set.
    let orphan_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "auto-parent",
            "--var",
            "topic=orphan",
            "--no-parent",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .env("COSMON_PARENT_MOL_ID", &parent_id)
        .output()
        .expect("nucleate orphan failed");
    assert!(orphan_out.status.success());
    let orphan_id = serde_json::from_str::<serde_json::Value>(
        String::from_utf8_lossy(&orphan_out.stdout).trim(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let orphan_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&orphan_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    // `typed_links` is serialized with `skip_serializing_if = Vec::is_empty`,
    // so an absent field is equivalent to an empty list.
    let orphan_links = orphan_state
        .get("typed_links")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        orphan_links.is_empty(),
        "--no-parent must suppress the auto-link, got: {orphan_links:?}"
    );

    // Step 6 — explicit --blocked-by must silence the env contract entirely
    // (i.e. no DecayedFrom is added on top of the explicit edge).
    let explicit_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "auto-parent",
            "--var",
            "topic=explicit",
            "--blocked-by",
            &parent_id,
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .env("COSMON_PARENT_MOL_ID", &parent_id)
        .output()
        .expect("nucleate explicit failed");
    assert!(explicit_out.status.success());
    let explicit_id = serde_json::from_str::<serde_json::Value>(
        String::from_utf8_lossy(&explicit_out.stdout).trim(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let explicit_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&explicit_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let explicit_links = explicit_state["typed_links"].as_array().unwrap();
    assert_eq!(explicit_links.len(), 1, "only the explicit BlockedBy edge");
    assert_eq!(explicit_links[0]["rel"], "blocked_by");
}

/// `cs nucleate --decayed-from <id>` makes the information edge first-class
/// without requiring an env var. The child gets `DecayedFrom`, the parent
/// gains a symmetric `DecayProduct`.
#[test]
fn test_nucleate_explicit_decayed_from_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "df-test"
version = 1
description = "decayed-from explicit"
id_prefix = "df"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;
    fs::write(formulas_dir.join("df-test.formula.toml"), formula_toml).unwrap();

    let parent_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "df-test",
            "--var",
            "topic=parent",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .env_remove("COSMON_PARENT_MOL_ID")
        .output()
        .expect("parent nucleate failed");
    let parent_id = serde_json::from_str::<serde_json::Value>(
        String::from_utf8_lossy(&parent_out.stdout).trim(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let child_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "df-test",
            "--var",
            "topic=child",
            "--decayed-from",
            &parent_id,
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .env_remove("COSMON_PARENT_MOL_ID")
        .output()
        .expect("child nucleate failed");
    assert!(
        child_out.status.success(),
        "explicit --decayed-from: {}",
        String::from_utf8_lossy(&child_out.stderr)
    );
    let child_id = serde_json::from_str::<serde_json::Value>(
        String::from_utf8_lossy(&child_out.stdout).trim(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let child_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&child_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let child_links = child_state["typed_links"].as_array().unwrap();
    assert_eq!(child_links.len(), 1);
    assert_eq!(child_links[0]["rel"], "decayed_from");
    assert_eq!(child_links[0]["id"], parent_id);

    let parent_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&parent_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let parent_links = parent_state["typed_links"].as_array().unwrap();
    assert_eq!(parent_links.len(), 1);
    assert_eq!(parent_links[0]["rel"], "decay_product");
    assert_eq!(parent_links[0]["id"], child_id);
}

/// Regression guard for the deep-think path-1 bug:
/// nucleating children from a deliberation's outcomes step MUST wire the
/// typed `BlockedBy` edge back to the parent, not merely reference the
/// parent id in free text. This test simulates the canonical shape of that
/// call sequence — nucleate a parent deliberation-like molecule, nucleate
/// N children with `--blocked-by <parent>`, then assert that
/// `cs deps <parent> --transitive --json` returns all N children in the
/// downstream closure.
///
/// If a future refactor drops the symmetric link maintenance or a formula
/// edit weakens the requirement, this test fails loudly instead of letting
/// the DAG degrade back into textual-only lineage.
#[test]
#[allow(clippy::too_many_lines)]
fn test_deps_transitive_catches_orphaned_deliberation_children() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "delib-regress"
version = 1
description = "deliberation child linking regression"
id_prefix = "dlr"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;
    fs::write(
        formulas_dir.join("delib-regress.formula.toml"),
        formula_toml,
    )
    .unwrap();

    // (a) Nucleate the parent — stands in for the deliberation molecule.
    let parent_out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "delib-regress",
            "--var",
            "topic=parent-delib",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate parent failed");
    assert!(
        parent_out.status.success(),
        "nucleate parent: {}",
        String::from_utf8_lossy(&parent_out.stderr)
    );
    let parent_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&parent_out.stdout).trim()).unwrap();
    let parent_id = parent_json["id"].as_str().unwrap().to_owned();

    // (b) Nucleate three children, each with --blocked-by parent — this is
    // exactly the pattern deep-think path (1) prescribes.
    let mut child_ids: Vec<String> = Vec::with_capacity(3);
    for topic in ["child-one", "child-two", "child-three"] {
        let child_out = cosmon_bin()
            .args([
                "--json",
                "nucleate",
                "delib-regress",
                "--var",
                &format!("topic={topic}"),
                "--blocked-by",
                &parent_id,
                "--store-dir",
                state_dir.to_str().unwrap(),
                "--formulas-dir",
                formulas_dir.to_str().unwrap(),
            ])
            .output()
            .expect("nucleate child failed");
        assert!(
            child_out.status.success(),
            "nucleate {topic}: {}",
            String::from_utf8_lossy(&child_out.stderr)
        );
        let child_json: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&child_out.stdout).trim()).unwrap();
        child_ids.push(child_json["id"].as_str().unwrap().to_owned());
    }

    // (c) `cs deps parent --transitive --json` must list all three children
    // in downstream. This is the assertion that would have caught
    // delib-20260409-b22c: if --blocked-by is missed, downstream is empty
    // even though the children physically exist.
    let deps_out = cosmon_bin()
        .args([
            "--config",
            state_dir.to_str().unwrap(),
            "--json",
            "deps",
            &parent_id,
            "--transitive",
        ])
        .output()
        .expect("cs deps failed");
    assert!(
        deps_out.status.success(),
        "cs deps: {}",
        String::from_utf8_lossy(&deps_out.stderr)
    );
    let deps_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&deps_out.stdout).trim()).unwrap();
    assert_eq!(deps_json["transitive"], true);
    let downstream: Vec<String> = deps_json["downstream"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        downstream.len(),
        3,
        "expected 3 downstream children, got: {downstream:?}"
    );
    for cid in &child_ids {
        assert!(
            downstream.contains(cid),
            "child {cid} missing from transitive downstream closure {downstream:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// cs wait — bounded poll until a molecule reaches a terminal status
// ---------------------------------------------------------------------------

/// Canonical success path: `cs complete` → `cs wait` returns immediately with
/// zero wasted polls because the molecule is already terminal on the first
/// read. Exercises both the JSON surface and the `already-terminal ⇒ no sleep`
/// invariant that the trinity `cs tackle && cs wait && cs done` depends on.
#[test]
fn test_cs_wait_returns_immediately_on_terminal_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "wait-test"
version = 1
description = "Wait integration test"
id_prefix = "wt"

[[steps]]
id = "only"
title = "Only step"
description = "Just one step"
acceptance = "done"
"#;
    fs::write(formulas_dir.join("wait-test.formula.toml"), formula_toml).unwrap();

    // Nucleate a molecule we can complete and then wait on.
    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "wait-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        out.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let nuc: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let mol_id = nuc["id"].as_str().unwrap().to_owned();

    // Complete it directly (no worker, no evolve ceremony).
    let out = cosmon_bin()
        .args([
            "complete",
            &mol_id,
            "--ops-dir",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("complete failed");
    assert!(
        out.status.success(),
        "complete: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `cs wait` on an already-terminal molecule must return without polling.
    // We assert this on the *signal* (`poll_count`), not on wall-clock time:
    // the wait kernel reports `poll_count == 1` when the molecule was already
    // in the target set on the first store read (one read, no loop), and
    // `>= 2` once it has slept at least one poll interval. Wall-clock is the
    // wrong instrument here — under a saturated `cargo test --workspace` run,
    // process cold-start alone can take >15s, so any absolute or
    // spawn-relative time bound flakes without telling us anything about
    // whether the wait actually polled. (`poll_count` is also exactly what
    // the kernel test `test_immediate_return_when_already_terminal` asserts
    // via its sleep counter.)
    let out = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "wait",
            &mol_id,
            "--timeout",
            "30",
            "--poll-interval",
            "1",
        ])
        .output()
        .expect("wait failed");
    assert!(
        out.status.success(),
        "wait should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wait_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(
        wait_json["poll_count"], 1,
        "already-terminal wait should detect the terminal state on the first \
         read and not poll — poll_count={}",
        wait_json["poll_count"]
    );
    assert_eq!(wait_json["status"], "completed");
    assert_eq!(wait_json["reached"], "completed");
    assert_eq!(wait_json["molecule"], mol_id);
}

/// Timeout path: waiting for a status the molecule will never reach must
/// exit 124 (`timeout(1)` convention) with a structured error JSON.
#[test]
fn test_cs_wait_exits_124_on_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "wait-tmo"
version = 1
description = "Wait timeout test"
id_prefix = "wtm"

[[steps]]
id = "only"
title = "Only"
description = "Only"
acceptance = "done"
"#;
    fs::write(formulas_dir.join("wait-tmo.formula.toml"), formula_toml).unwrap();

    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "wait-tmo",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(out.status.success());
    let nuc: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let mol_id = nuc["id"].as_str().unwrap().to_owned();

    // Wait for `running` — a pending, unassigned molecule will never reach it.
    let out = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "wait",
            &mol_id,
            "--for",
            "running",
            "--timeout",
            "1",
            "--poll-interval",
            "1",
        ])
        .output()
        .expect("wait failed");
    assert_eq!(
        out.status.code(),
        Some(124),
        "wait should exit 124 on timeout (matches timeout(1)): stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err_line = String::from_utf8_lossy(&out.stderr);
    let err_json: serde_json::Value = serde_json::from_str(err_line.trim())
        .unwrap_or_else(|e| panic!("stderr should be JSON: {e}\n{err_line}"));
    assert_eq!(err_json["error"], "timeout");
    assert_eq!(err_json["molecule"], mol_id);
    assert_eq!(err_json["last_status"], "pending");
}

/// `cs nucleate task-work --kind deliberation` must parse and persist the
/// new kind end-to-end. Guards the parser + CLI + state store wiring for
/// the Deliberation variant.
#[test]
fn test_nucleate_kind_deliberation() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    // Reuse the task-work formula shape for isolation from repo state.
    let formula_toml = r#"
formula = "task-work"
version = 1
description = "Delib kind test formula"
id_prefix = "delib"

[[steps]]
id = "implement"
title = "Implement"
description = "Do it."
acceptance = "Done"

[[steps]]
id = "verify"
title = "Verify"
description = "Check it."
needs = ["implement"]
acceptance = "Checked"
"#;
    fs::write(formulas_dir.join("task-work.formula.toml"), formula_toml).unwrap();

    let output = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--kind",
            "deliberation",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate --kind deliberation failed");
    assert!(
        output.status.success(),
        "nucleate --kind deliberation should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("JSON output invalid: {e}\n{stdout}"));
    let mol_id = parsed["id"].as_str().expect("id should be present");
    assert!(
        mol_id.starts_with("delib-"),
        "id should use formula id_prefix: {mol_id}"
    );

    // The persisted molecule must carry kind=deliberation.
    let state_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(mol_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state_json["kind"], "deliberation");
}

/// Part XVIII coupling invariant: `cs observe --json <id>` and
/// `cs wait --json <id>` must return the **same** energy aggregation for
/// a molecule that has records in `log/energy.jsonl`. This is the shared
/// kernel test — if either verb drifts, operators see two different answers
/// to the same question ("what did this molecule cost?") and the metric
/// coupling principle is broken.
#[test]
#[allow(clippy::too_many_lines)]
fn test_cs_observe_and_wait_return_same_energy_aggregation() {
    use chrono::Utc;

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "cpl-obs-wait"
version = 1
description = "Coupling observe/wait energy test"
id_prefix = "cpl"

[[steps]]
id = "only"
title = "Only"
description = "Only"
acceptance = "done"
"#;
    fs::write(formulas_dir.join("cpl-obs-wait.formula.toml"), formula_toml).unwrap();

    // Nucleate a molecule — starts in `pending`, which is a valid wait
    // target so `cs wait --for pending` returns immediately.
    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "cpl-obs-wait",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        out.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let nuc: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let mol_id = nuc["id"].as_str().unwrap().to_owned();

    // Seed the energy log on disk with two records for this molecule and
    // one for a different molecule (which must be filtered out by both
    // verbs). The expected aggregation is 2200 in / 800 out / 0.0275.
    let log_dir = state_dir.join("log");
    fs::create_dir_all(&log_dir).unwrap();
    let log_path = log_dir.join("energy.jsonl");
    let now = Utc::now().to_rfc3339();
    let record = |mol: &str, i: u64, o: u64, c: f64| {
        format!(
            "{{\"timestamp\":\"{now}\",\"worker\":\"topaz\",\"molecule\":\"{mol}\",\"step\":\"only\",\"model\":\"claude-opus-4-6\",\"input_tokens\":{i},\"output_tokens\":{o},\"cost\":{c}}}\n"
        )
    };
    let body = format!(
        "{}{}{}",
        record(&mol_id, 1500, 500, 0.0200),
        record(&mol_id, 700, 300, 0.0075),
        record("cpl-20260409-xxxx", 9999, 9999, 1.0000),
    );
    fs::write(&log_path, body).unwrap();

    // `cs observe <id> --json` — snapshot with the coupling report.
    let out = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "observe",
            &mol_id,
        ])
        .output()
        .expect("observe failed");
    assert!(
        out.status.success(),
        "observe: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let observe_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();

    // `cs wait <id> --for pending` — already in target, returns immediately.
    let out = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "wait",
            &mol_id,
            "--for",
            "pending",
            "--timeout",
            "5",
            "--poll-interval",
            "1",
        ])
        .output()
        .expect("wait failed");
    assert!(
        out.status.success(),
        "wait: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wait_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();

    // Invariant 1: both verbs expose the coupling-report scalars.
    assert_eq!(
        observe_json["poll_count"], 1,
        "snapshot must report poll_count=1"
    );
    assert_eq!(
        observe_json["transitions"], 0,
        "snapshot must report transitions=0"
    );
    assert!(wait_json.get("poll_count").is_some());
    assert!(wait_json.get("transitions").is_some());

    // Invariant 2: the energy aggregation is identical between the two verbs.
    // This is the contract of Part XVIII — the shared kernel in cosmon-state.
    let observe_energy = &observe_json["energy"];
    let wait_energy = &wait_json["energy"];
    assert!(
        observe_energy.is_object(),
        "observe should surface energy when log exists: {observe_json}"
    );
    assert!(
        wait_energy.is_object(),
        "wait should surface energy when log exists: {wait_json}"
    );
    assert_eq!(
        observe_energy, wait_energy,
        "observe and wait must return bit-identical energy aggregations"
    );
    assert_eq!(observe_energy["input_tokens"], 2200);
    assert_eq!(observe_energy["output_tokens"], 800);
    let cost = observe_energy["cost_usd"].as_f64().unwrap();
    assert!((cost - 0.0275).abs() < 1e-9);

    // Invariant 3: entropy and temperature stay absent (omit-if-none) — the
    // cognitive-SNR ceiling is a hard cap. Silent widening would break the
    // Shannon bound from the delib-20260409-b22c synthesis.
    assert!(
        observe_json.get("entropy").is_none(),
        "entropy has no probe yet — must omit from the wire format"
    );
    assert!(
        observe_json.get("temperature").is_none(),
        "temperature has no probe yet — must omit from the wire format"
    );
}

/// Per-molecule API token tracking (task-20260625-d1fa): `cs observe
/// <id> --json` must surface an `api_tokens` object summed from the
/// canonical token-meter sink keyed by `molecule_id`, and omit it when
/// no event matches.
#[test]
fn test_cs_observe_surfaces_per_molecule_api_tokens() {
    use chrono::Utc;

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "tok-obs"
version = 1
description = "Per-molecule token tracking test"
id_prefix = "tok"

[[steps]]
id = "only"
title = "Only"
description = "Only"
acceptance = "done"
"#;
    fs::write(formulas_dir.join("tok-obs.formula.toml"), formula_toml).unwrap();

    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "tok-obs",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        out.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let nuc: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let mol_id = nuc["id"].as_str().unwrap().to_owned();

    // Seed the canonical token-meter sink: two calls for this molecule
    // and one for an unrelated molecule (must be filtered out). Expected
    // fold: 1500 in / 500 out / 1200 micros / 2 invocations.
    let instr_dir = state_dir.join("instrumentation");
    fs::create_dir_all(&instr_dir).unwrap();
    let now = Utc::now().to_rfc3339();
    let ev = |mol: &str, tin: u64, tout: u64, cost: u64| {
        format!(
            "{{\"tenant\":\"operator\",\"molecule_id\":\"{mol}\",\"backend\":\"anthropic\",\"tokens_in\":{tin},\"tokens_out\":{tout},\"cost_micros_estimated\":{cost},\"timestamp\":\"{now}\"}}\n"
        )
    };
    let body = format!(
        "{}{}{}",
        ev(&mol_id, 1000, 400, 900),
        ev(&mol_id, 500, 100, 300),
        ev("tok-20260101-xxxx", 9999, 9999, 9999),
    );
    fs::write(instr_dir.join("tokens.jsonl"), body).unwrap();

    let out = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "observe",
            &mol_id,
        ])
        .output()
        .expect("observe failed");
    assert!(
        out.status.success(),
        "observe: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let observe_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();

    let api = &observe_json["api_tokens"];
    assert!(
        api.is_object(),
        "observe should surface api_tokens when the sink has matching events: {observe_json}"
    );
    assert_eq!(api["tokens_in"], 1500);
    assert_eq!(api["tokens_out"], 500);
    assert_eq!(api["cost_micros_estimated"], 1200);
    assert_eq!(api["invocations"], 2);

    // A molecule with no recorded events must omit the field (omit-if-none).
    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "tok-obs",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate 2 failed");
    let nuc2: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let mol_id2 = nuc2["id"].as_str().unwrap().to_owned();
    let out = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "observe",
            &mol_id2,
        ])
        .output()
        .expect("observe 2 failed");
    let observe_json2: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert!(
        observe_json2.get("api_tokens").is_none(),
        "api_tokens must be omitted when no event matches the molecule: {observe_json2}"
    );
}

/// `cs tokens --molecule <id> --json` folds the canonical sink to a
/// single per-molecule row, filtering out unrelated molecules.
#[test]
fn test_cs_tokens_molecule_filter() {
    use chrono::Utc;

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let instr_dir = state_dir.join("instrumentation");
    fs::create_dir_all(&instr_dir).unwrap();

    let now = Utc::now().to_rfc3339();
    let ev = |mol: &str, tin: u64, tout: u64, cost: u64| {
        format!(
            "{{\"tenant\":\"operator\",\"molecule_id\":\"{mol}\",\"backend\":\"anthropic\",\"tokens_in\":{tin},\"tokens_out\":{tout},\"cost_micros_estimated\":{cost},\"timestamp\":\"{now}\"}}\n"
        )
    };
    let body = format!(
        "{}{}{}",
        ev("task-20260625-aaaa", 1000, 400, 900),
        ev("task-20260625-aaaa", 500, 100, 300),
        ev("task-20260625-bbbb", 7, 7, 7),
    );
    fs::write(instr_dir.join("tokens.jsonl"), body).unwrap();

    let out = cosmon_bin()
        .args([
            "--config",
            state_dir.to_str().unwrap(),
            "tokens",
            "--molecule",
            "task-20260625-aaaa",
            "--since",
            "365d",
            "--json",
        ])
        .output()
        .expect("tokens failed");
    assert!(
        out.status.success(),
        "tokens: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let row: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(row["molecule"], "task-20260625-aaaa");
    assert_eq!(row["tokens_in"], 1500);
    assert_eq!(row["tokens_out"], 500);
    assert_eq!(row["cost_micros_estimated"], 1200);
    assert_eq!(row["invocations"], 2);
}

/// Unknown-molecule path: `cs wait bogus-id` must fail fast with exit 1 —
/// polling cannot recover from a missing molecule.
#[test]
fn test_cs_wait_rejects_missing_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");

    let out = cosmon_bin()
        .args([
            "--config",
            state_dir.to_str().unwrap(),
            "wait",
            "task-20260409-gone",
            "--timeout",
            "1",
        ])
        .output()
        .expect("wait failed");
    assert!(!out.status.success());
    assert_ne!(
        out.status.code(),
        Some(124),
        "missing molecule is a hard error, not a timeout"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("not found") || combined.contains("task-20260409-gone"),
        "error should name the missing molecule: {combined}"
    );
}

/// Flip a nucleated (pending) molecule to running status by rewriting its
/// state.json on disk. Replaces the `cs dispatch` transition that retired
/// with ADR-052 §CLI delta.
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

/// `cs evolve` wires `VerificationSpec`: when a step has `[steps.verification]`
/// with a `criteria` command, evolve runs it as a shell gate. If the command
/// fails and retries remain, the step does NOT advance. If retries are
/// exhausted (`max_retries=0`), the molecule is marked stuck (frozen).
#[test]
#[allow(clippy::too_many_lines)]
fn test_evolve_verification_spec_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    // Gate file — verification criteria checks for its existence.
    let gate_file = tmp.path().join("gate-marker");

    let formula_toml = format!(
        r#"
formula = "verify-gate-test"
version = 1
description = "Formula with a verification gate on step 1"
id_prefix = "vg"

[[steps]]
id = "gated-step"
title = "Gated step"
description = "This step has a verification gate."
acceptance = "Gate passed"

[steps.verification]
criteria = "test -f {}"
max_retries = 1

[[steps]]
id = "final-step"
title = "Final step"
description = "After the gate."
acceptance = "Done"
needs = ["gated-step"]
"#,
        gate_file.display()
    );
    let formula_path = formulas_dir.join("verify-gate-test.formula.toml");
    fs::write(&formula_path, &formula_toml).unwrap();

    let state_str = state_dir.to_str().unwrap();

    // Nucleate the molecule (replaces spawn + dispatch post-ADR-052).
    let output = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "verify-gate-test",
            "--store-dir",
            state_str,
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        output.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap();
    mark_molecule_running(&state_dir, molecule_id);

    // ── Attempt 1: evolve WITHOUT gate file — verification should fail,
    //    step should NOT advance (retries remaining = 1).
    let output = cosmon_bin()
        .args([
            "--json",
            "evolve",
            molecule_id,
            "--evidence",
            "Gate passed (attempting)",
            "--ops-dir",
            state_str,
            "--formula",
            formula_path.to_str().unwrap(),
        ])
        .output()
        .expect("evolve attempt 1 failed");
    assert!(
        output.status.success(),
        "evolve should return Ok even when verification fails with retries: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evolve1_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    assert_eq!(
        evolve1_json["verification_failed"], true,
        "should report verification_failed"
    );
    assert_eq!(
        evolve1_json["retries_remaining"], 1,
        "should report 1 retry remaining"
    );

    // Verify molecule state was NOT advanced (still on step 0).
    let mol_dir = state_dir.join("fleets/default/molecules").join(molecule_id);
    let mol_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(mol_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(
        mol_state["current_step"], 0,
        "step should not have advanced"
    );

    // ── Attempt 2: create the gate file, evolve again — should pass.
    fs::write(&gate_file, "present").unwrap();

    let output = cosmon_bin()
        .args([
            "--json",
            "evolve",
            molecule_id,
            "--evidence",
            "Gate passed for real this time",
            "--ops-dir",
            state_str,
            "--formula",
            formula_path.to_str().unwrap(),
        ])
        .output()
        .expect("evolve attempt 2 failed");
    assert!(
        output.status.success(),
        "evolve should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evolve2_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    assert_eq!(
        evolve2_json["completed_step"], "gated-step",
        "should complete the gated step"
    );
    assert_eq!(
        evolve2_json["new_step"],
        serde_json::json!("final-step"),
        "should advance to final step"
    );
}

/// When `max_retries = 0` and verification fails, the molecule is immediately
/// marked stuck (frozen) — no retry window.
#[test]
fn test_evolve_verification_exhausted_marks_stuck() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "no-retry-test"
version = 1
description = "Formula with verification but zero retries"
id_prefix = "nr"

[[steps]]
id = "doomed"
title = "Doomed step"
description = "Verification will fail with no retries."
acceptance = "Impossible"

[steps.verification]
criteria = "false"
max_retries = 0
"#;
    let formula_path = formulas_dir.join("no-retry-test.formula.toml");
    fs::write(&formula_path, formula_toml).unwrap();

    let state_str = state_dir.to_str().unwrap();

    // Nucleate the molecule (replaces spawn + dispatch post-ADR-052).
    let output = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "no-retry-test",
            "--store-dir",
            state_str,
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(output.status.success());
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap();
    mark_molecule_running(&state_dir, molecule_id);

    // Evolve — verification fails, 0 retries → stuck.
    let output = cosmon_bin()
        .args([
            "--json",
            "evolve",
            molecule_id,
            "--evidence",
            "Attempting impossible verification",
            "--ops-dir",
            state_str,
            "--formula",
            formula_path.to_str().unwrap(),
        ])
        .output()
        .expect("evolve failed");
    assert!(
        output.status.success(),
        "evolve should still return Ok: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evolve_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    assert_eq!(evolve_json["verification_failed"], true);
    assert_eq!(evolve_json["stuck"], true);

    // Verify molecule is now frozen.
    let mol_dir = state_dir.join("fleets/default/molecules").join(molecule_id);
    let mol_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(mol_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(mol_state["status"], "frozen", "molecule should be frozen");
}

// ---------- TTL / Expiry CLI integration tests (ADR-029) -----------------

fn setup_ttl_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    let formula_toml = r#"
formula = "ttl-test"
version = 1
description = "TTL test"
id_prefix = "ttl"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;
    fs::write(formulas_dir.join("ttl-test.formula.toml"), formula_toml).unwrap();
    (tmp, state_dir, formulas_dir)
}

fn nucleate_with(
    args: &[&str],
    state_dir: &std::path::Path,
    formulas_dir: &std::path::Path,
) -> serde_json::Value {
    let mut cmd = cosmon_bin();
    cmd.args(["--json", "nucleate", "ttl-test"]);
    cmd.args(args);
    cmd.args([
        "--store-dir",
        state_dir.to_str().unwrap(),
        "--formulas-dir",
        formulas_dir.to_str().unwrap(),
    ]);
    let out = cmd.output().expect("nucleate failed");
    assert!(
        out.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap()
}

fn load_state(state_dir: &std::path::Path, id: &str) -> serde_json::Value {
    let path = state_dir
        .join("fleets/default/molecules")
        .join(id)
        .join("state.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn test_nucleate_ttl_sets_expires_at() {
    let (_tmp, state_dir, formulas_dir) = setup_ttl_fixture();
    let j = nucleate_with(
        &["--ttl", "7d", "--expiry-policy", "collapse"],
        &state_dir,
        &formulas_dir,
    );
    let id = j["id"].as_str().unwrap();
    let state = load_state(&state_dir, id);
    assert!(state["expires_at"].is_string(), "expires_at should be set");
    assert_eq!(state["expiry_policy"], "collapse");
}

#[test]
fn test_nucleate_expires_at_date() {
    let (_tmp, state_dir, formulas_dir) = setup_ttl_fixture();
    let j = nucleate_with(&["--expires-at", "2099-07-02"], &state_dir, &formulas_dir);
    let id = j["id"].as_str().unwrap();
    let state = load_state(&state_dir, id);
    let ts = state["expires_at"].as_str().unwrap();
    assert!(ts.starts_with("2099-07-02"), "got {ts}");
}

#[test]
fn test_nucleate_rejects_ttl_and_expires_at_together() {
    let (_tmp, state_dir, formulas_dir) = setup_ttl_fixture();
    let out = cosmon_bin()
        .args([
            "nucleate",
            "ttl-test",
            "--ttl",
            "7d",
            "--expires-at",
            "2099-07-02",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("run failed");
    assert!(!out.status.success());
}

// test_touch_* and test_expire_* were retired with the cs touch / cs expire
// verbs in ADR-052 §CLI delta. TTL fields on nucleate are still covered above
// (test_nucleate_ttl_sets_expires_at / _expires_at_date / _rejects_*_together).

/// `cs init` does not run `git init` — git lifecycle is the user's job.
/// The target directory exists after `cs init`, `.cosmon/` is populated,
/// but `.git/` is absent unless the user created it themselves. Scripts
/// (quickstart-wikipedia.sh and friends) must call `git init` and stamp
/// an initial commit before `cs tackle` can create a branch.
#[test]
fn test_init_does_not_run_git_init() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("no-git-proj");

    // Path intentionally does NOT exist — exercises the bootstrap primitive.
    assert!(!target.exists(), "precondition: target must not exist");

    let output = cosmon_bin()
        .args(["init", "--yes", target.to_str().unwrap()])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("init failed");
    assert!(
        output.status.success(),
        "cs init on non-existent path must succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(target.is_dir(), "target directory must be created");
    assert!(
        target.join(".cosmon").is_dir(),
        ".cosmon/ must be populated"
    );
    assert!(
        !target.join(".git").exists(),
        "cs init must NOT create .git/ — that is git's job"
    );

    // The user stamps an initial commit themselves when they need one.
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&target)
        .status()
        .expect("git init failed");
    assert!(init.success(), "user's git init should succeed");
    // Hermetic identity: a bare CI runner has no global git user, and this
    // assert is about cs staying out of git's way, not about the runner's
    // dotfiles.
    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=cosmon-test",
            "-c",
            "user.email=test@cosmon.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "initial commit",
        ])
        .current_dir(&target)
        .output()
        .expect("git commit failed");
    assert!(commit.status.success(), "initial commit should succeed");
}

/// The quickstart-wikipedia.sh script must create an initial git commit
/// before calling `cs nucleate` + `cs tackle`. This test validates the
/// script's bash syntax and verifies the commit step is present.
#[test]
fn test_quickstart_script_has_initial_commit_step() {
    let script = include_str!("../../../scripts/quickstart-wikipedia.sh");

    // The script must contain a `git commit` call before the Dispatch section.
    let dispatch_marker = "# ── Dispatch";
    let commit_marker = "git commit -m";
    let add_marker = "git add -A";

    let dispatch_pos = script
        .find(dispatch_marker)
        .expect("script should have a Dispatch section");
    let commit_pos = script
        .find(commit_marker)
        .expect("script should contain git commit");
    let add_pos = script
        .find(add_marker)
        .expect("script should contain git add -A");

    assert!(
        add_pos < dispatch_pos,
        "git add -A must appear before the Dispatch section"
    );
    assert!(
        commit_pos < dispatch_pos,
        "git commit must appear before the Dispatch section"
    );
    assert!(
        add_pos < commit_pos,
        "git add must appear before git commit"
    );
}

/// End-to-end auto-register hook: `cs nucleate` invoked inside an unknown
/// git repo must append a hint line to the neurion JSONL file, and that
/// hint must become a row in the `repos` table after the drain runs.
#[test]
fn test_nucleate_emits_neurion_auto_register_hint() {
    let tmp = tempfile::tempdir().unwrap();

    // Create a real git repo — find_repo_root requires `.git` as a dir.
    let repo = tmp.path().join("fakerepo");
    fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .expect("git init");
    assert!(status.success());

    // Minimal single-step formula.
    let formulas_dir = repo.join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    let formula_toml = r#"
formula = "hint-test"
version = 1
description = "Auto-register hint integration test"
id_prefix = "hint"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;
    fs::write(formulas_dir.join("hint-test.formula.toml"), formula_toml).unwrap();

    let state_dir = tmp.path().join("state");
    let hint_file = tmp.path().join("auto-register.jsonl");

    // Run `cs nucleate` from inside the repo so cwd detection kicks in.
    let output = cosmon_bin()
        .current_dir(&repo)
        .env("NEURION_AUTO_REGISTER_FILE", &hint_file)
        .args([
            "--json",
            "nucleate",
            "hint-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        output.status.success(),
        "nucleate should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Hint file exists and contains one well-formed line.
    assert!(
        hint_file.exists(),
        "auto-register.jsonl should have been created at {}",
        hint_file.display()
    );
    let content = fs::read_to_string(&hint_file).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one hint expected, got: {content}");
    let hint: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("hint should parse as JSON: {e}\n{}", lines[0]));
    assert_eq!(hint["name"], "fakerepo");
    assert_eq!(hint["source"], "cosmon:nucleate");
    // local_path may be canonicalized — just assert it ends with the repo basename.
    let local_path = hint["local_path"].as_str().unwrap();
    assert!(
        local_path.ends_with("fakerepo"),
        "local_path should end with repo name, got {local_path}"
    );
}

/// `cs nucleate` invoked inside a `.worktrees/` subtree must NOT emit a
/// hint — that would register the worktree itself, creating recursion.
#[test]
fn test_nucleate_skips_hint_inside_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("mainrepo");
    let wt = repo.join(".worktrees").join("task-x");
    fs::create_dir_all(&wt).unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .expect("git init");
    assert!(status.success());

    let formulas_dir = wt.join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        formulas_dir.join("wt-test.formula.toml"),
        r#"
formula = "wt-test"
version = 1
description = "Worktree skip"
id_prefix = "wt"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#,
    )
    .unwrap();

    let state_dir = tmp.path().join("state");
    let hint_file = tmp.path().join("auto-register.jsonl");

    let output = cosmon_bin()
        .current_dir(&wt)
        .env("NEURION_AUTO_REGISTER_FILE", &hint_file)
        .args([
            "--json",
            "nucleate",
            "wt-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(output.status.success());

    assert!(
        !hint_file.exists(),
        "hint file must NOT be created from a worktree path"
    );
}

/// End-to-end: `cs nucleate` stamps a `prompt_seal` on the new molecule
/// and `cs verify` reports it as PASS. Tampering with `prompt.md`
/// afterwards flips the same seal check to FAIL — the core "shadow
/// contract" smoke-alarm behaviour.
#[test]
#[allow(clippy::too_many_lines)]
fn test_prompt_seal_round_trip_and_tamper_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "seal-smoke-test"
version = 1
description = "Minimal formula used to exercise the soft-contract hash seal"
id_prefix = "ss"

[[steps]]
id = "only-step"
title = "Only step"
description = "One-shot step — just enough to persist a molecule."
acceptance = "done"
"#;
    let formula_path = formulas_dir.join("seal-smoke-test.formula.toml");
    fs::write(&formula_path, formula_toml).unwrap();

    // Nucleate — the post-write hook should stamp `prompt_seal`.
    let output = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "seal-smoke-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
            "--var",
            "topic=hash seals",
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        output.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap();
    let mol_dir = state_dir.join("fleets/default/molecules").join(molecule_id);

    // State must carry a populated `prompt_seal`.
    let state_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(mol_dir.join("state.json")).unwrap()).unwrap();
    let prompt_seal = &state_json["prompt_seal"];
    assert!(
        prompt_seal.is_object(),
        "prompt_seal should be populated: {state_json}"
    );
    assert_eq!(prompt_seal["step"], 0);
    let seal_hash = prompt_seal["hash"].as_str().expect("hash is str");
    assert_eq!(seal_hash.len(), 64, "blake3 hex must be 64 chars");

    // `cs verify` must report the prompt seal as PASS.
    let output = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "verify",
            molecule_id,
            "--no-replay",
        ])
        .output()
        .expect("verify failed");
    // Exit 0 (clean) or 2 (inconclusive — no artifact-seal yet) both
    // indicate no tampering. Exit 1 (fail) would mean our prompt seal
    // mismatched, which is the bug we are specifically guarding against.
    assert!(
        matches!(output.status.code(), Some(0 | 2)),
        "verify should not fail on clean molecule: exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let verify_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let seal_rows: Vec<_> = verify_json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["category"] == "seal")
        .collect();
    assert!(
        seal_rows
            .iter()
            .any(|c| c["name"] == "prompt.md" && c["status"] == "PASS"),
        "prompt seal must PASS on unmodified prompt.md: {verify_json}"
    );

    // Tamper with prompt.md — the shadow-contract smoke alarm must fire.
    fs::write(mol_dir.join("prompt.md"), "edited after nucleation\n").unwrap();

    let output = cosmon_bin()
        .args([
            "--json",
            "--config",
            state_dir.to_str().unwrap(),
            "verify",
            molecule_id,
            "--no-replay",
        ])
        .output()
        .expect("verify failed");
    assert_eq!(
        output.status.code(),
        Some(1),
        "verify must exit 1 after prompt.md tamper: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let verify_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    assert_eq!(verify_json["status"], "fail");
    assert!(
        verify_json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["category"] == "seal" && c["name"] == "prompt.md" && c["status"] == "FAIL"),
        "prompt seal must FAIL after tamper: {verify_json}"
    );
}

// ---------- ADR-052 §D3 — CLI rename/merge deprecation notices ----------
//
// Each deprecated alias must emit a stderr notice pointing to its canonical
// successor. Behaviour-identical to the old verb for one release cycle.

#[test]
fn test_harvest_deprecation_notice_mentions_done_if_completed() {
    let tmp = tempfile::tempdir().unwrap();
    // cs harvest is expected to error (no molecule exists), but the stderr
    // must still carry the deprecation notice before the error.
    let output = cosmon_bin()
        .args([
            "--config",
            tmp.path().to_str().unwrap(),
            "harvest",
            "--molecule",
            "task-20260419-deprec",
        ])
        .output()
        .expect("failed to run cs harvest");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cs harvest: deprecated"),
        "stderr must carry deprecation notice: {stderr}"
    );
    assert!(
        stderr.contains("cs done --if-completed"),
        "notice must point to canonical replacement: {stderr}"
    );
    assert!(
        stderr.contains("ADR-052"),
        "notice must cite governing ADR: {stderr}"
    );
}

#[test]
fn test_kill_deprecation_notice_mentions_purge_force() {
    let tmp = tempfile::tempdir().unwrap();
    let output = cosmon_bin()
        .args([
            "--config",
            tmp.path().to_str().unwrap(),
            "kill",
            "ghost-worker",
        ])
        .output()
        .expect("failed to run cs kill");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cs kill: deprecated"),
        "stderr must carry deprecation notice: {stderr}"
    );
    assert!(
        stderr.contains("cs purge") && stderr.contains("--force"),
        "notice must point to canonical replacement: {stderr}"
    );
    assert!(
        stderr.contains("ADR-052"),
        "notice must cite governing ADR: {stderr}"
    );
}

#[test]
fn test_quench_deprecation_notice_mentions_freeze_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let output = cosmon_bin()
        .args([
            "--config",
            tmp.path().to_str().unwrap(),
            "quench",
            "ghost-worker",
            "--no-tmux",
        ])
        .output()
        .expect("failed to run cs quench");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cs quench: deprecated"),
        "stderr must carry deprecation notice: {stderr}"
    );
    assert!(
        stderr.contains("cs freeze") && stderr.contains("--reason"),
        "notice must point to canonical replacement: {stderr}"
    );
    assert!(
        stderr.contains("ADR-052"),
        "notice must cite governing ADR: {stderr}"
    );
}

#[test]
fn test_reconcile_deprecation_notice_mentions_project() {
    // cs reconcile in a temp dir with no surfaces.toml is a no-op success
    // (prints "No .cosmon/surfaces.toml found") — the deprecation notice
    // must still reach stderr regardless of the outcome.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let output = cosmon_bin()
        .args(["--config", state_dir.to_str().unwrap(), "reconcile"])
        .output()
        .expect("failed to run cs reconcile");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cs reconcile: deprecated"),
        "stderr must carry deprecation notice: {stderr}"
    );
    assert!(
        stderr.contains("cs project"),
        "notice must point to canonical replacement: {stderr}"
    );
    assert!(
        stderr.contains("ADR-052"),
        "notice must cite governing ADR: {stderr}"
    );
}

#[test]
fn test_project_is_canonical_no_deprecation_notice() {
    // Mirror of test_reconcile_deprecation_notice_mentions_project with
    // the canonical verb — must NOT emit any deprecation notice.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let output = cosmon_bin()
        .args(["--config", state_dir.to_str().unwrap(), "project"])
        .output()
        .expect("failed to run cs project");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("deprecated"),
        "canonical verb must not print deprecation notice: {stderr}"
    );
}

#[test]
fn test_done_accepts_if_completed_flag() {
    // `cs done --help` must advertise the `--if-completed` flag added
    // by ADR-052 §D3 as the canonical replacement for `cs harvest`.
    let output = cosmon_bin()
        .args(["done", "--help"])
        .output()
        .expect("failed to run cs done --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--if-completed"),
        "cs done --help must advertise --if-completed: {stdout}"
    );
}

#[test]
fn test_freeze_accepts_reason_flag() {
    let output = cosmon_bin()
        .args(["freeze", "--help"])
        .output()
        .expect("failed to run cs freeze --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--reason"),
        "cs freeze --help must advertise --reason: {stdout}"
    );
}

#[test]
fn test_purge_accepts_worker_and_force() {
    let output = cosmon_bin()
        .args(["purge", "--help"])
        .output()
        .expect("failed to run cs purge --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--force"),
        "cs purge --help must advertise --force: {stdout}"
    );
    assert!(
        stdout.contains("WORKER") || stdout.contains("worker"),
        "cs purge --help must document the positional worker: {stdout}"
    );
}
