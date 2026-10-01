// SPDX-License-Identifier: AGPL-3.0-only

//! `cs done` on a legacy merged molecule (issue #153).
//!
//! A state file written before the `archived` field (ADR-030 M3) deserializes
//! with `archived == false`. If it is `Completed` with `merged_at` set, the
//! work is already on the trunk, yet `cs status` and `cs peek --phase
//! harvestable` count it as awaiting harvest. The harvest command they print
//! (`cs done <id>`) must clear it: write the missing archive entry, set
//! `archived`, and say so. A `Completed` molecule without `merged_at` stays
//! harvestable.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// `cs` invocation pinned to an isolated state dir and run from inside the
/// project repo (so `find_repo_root()` resolves to the temp git repo).
fn cs_isolated(repo: &Path) -> Command {
    let state_dir = repo.join(".cosmon/state");
    let config_path = repo.join(".cosmon/config.toml");
    let mut cmd = cs();
    cmd.env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", &config_path)
        .current_dir(&state_dir);
    cmd
}

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    let mut full: Vec<&str> = vec!["-C", repo.to_str().unwrap()];
    full.extend_from_slice(args);
    Command::new("git")
        .args(&full)
        .output()
        .expect("git spawn failed")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Init a git repo with a `.cosmon` project whose state is gitignored and
/// whose archive subsystem is **enabled** — the gate the fix flows through.
fn setup_repo(tmp: &Path) {
    git_ok(tmp, &["init", "-q", "-b", "main"]);
    git_ok(tmp, &["config", "user.email", "test@example.com"]);
    git_ok(tmp, &["config", "user.name", "Test"]);
    git_ok(tmp, &["config", "commit.gpgsign", "false"]);

    let cosmon = tmp.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        "[project]\nproject_id = \"test-no-branch-archive\"\n\n[archive]\nenabled = true\n",
    )
    .unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    // One base commit so `main` exists and git topology probes resolve.
    git_ok(tmp, &["add", ".gitignore"]);
    git_ok(tmp, &["commit", "-q", "-m", "base"]);
}

/// Nucleate a `task-work` molecule without creating a feat branch.
fn nucleate_no_branch(repo: &Path) -> String {
    let nuc = cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=no_branch archive integration test",
        ])
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();

    mol_id
}

/// Drive a molecule to `Completed` without creating a feat branch. `cs
/// complete` does not archive it; that belongs to `cs done`.
fn nucleate_completed_no_branch(repo: &Path) -> String {
    let mol_id = nucleate_no_branch(repo);

    // `--ignore-mindguard`: this temp repo has no surface-verify gate machinery;
    // the hidden test escape hatch keeps the transition hermetic.
    let comp = cs_isolated(repo)
        .args([
            "--json",
            "complete",
            &mol_id,
            "--reason",
            "no_branch test",
            "--ignore-mindguard",
        ])
        .output()
        .expect("cs complete");
    assert!(
        comp.status.success(),
        "complete failed: {}",
        String::from_utf8_lossy(&comp.stderr)
    );
    mol_id
}

/// Walk `archive/YYYY/MM/` and return whether an entry for `mol_id` exists.
fn archive_entry_exists(state_dir: &Path, mol_id: &str) -> bool {
    let archive_root = state_dir.join("archive");
    if !archive_root.is_dir() {
        return false;
    }
    for year in fs::read_dir(&archive_root).into_iter().flatten().flatten() {
        if !year.path().is_dir() || year.file_name() == "events" {
            continue;
        }
        for month in fs::read_dir(year.path()).into_iter().flatten().flatten() {
            for entry in fs::read_dir(month.path()).into_iter().flatten().flatten() {
                if entry.file_name() == mol_id {
                    return entry.path().join("molecule.json").is_file();
                }
            }
        }
    }
    false
}

/// Rewrite the molecule's `state.json` into the legacy shape: `Completed`,
/// `merged_at` set, and no `archived` key at all.
fn make_legacy_merged(state_dir: &Path, mol_id: &str, merged: bool) {
    let path = state_dir
        .join("fleets/default/molecules")
        .join(mol_id)
        .join("state.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let obj = v.as_object_mut().unwrap();
    obj.remove("archived");
    // Rows of this age name the worker that produced them; its fleet entry is gone.
    obj.insert("assigned_worker".into(), format!("worker-{mol_id}").into());
    if merged {
        obj.insert("merged_at".into(), "2026-05-01T10:00:00Z".into());
    }
    fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

fn harvestable(repo: &Path) -> Vec<String> {
    let out = cs_isolated(repo)
        .args(["--json", "peek", "--phase", "harvestable"])
        .output()
        .expect("cs peek");
    assert!(
        out.status.success(),
        "peek failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .to_string()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn cs_done_clears_a_legacy_merged_molecule_from_harvestable() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let state_dir = repo.join(".cosmon/state");

    let merged = nucleate_completed_no_branch(repo);
    let unmerged = nucleate_completed_no_branch(repo);
    make_legacy_merged(&state_dir, &merged, true);
    make_legacy_merged(&state_dir, &unmerged, false);

    let before = harvestable(repo).join("\n");
    assert!(
        before.contains(&merged),
        "precondition: legacy row is counted:\n{before}"
    );

    let done = cs_isolated(repo)
        .args(["--json", "done", &merged, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    assert!(
        done.status.success(),
        "cs done failed: {stdout}{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim().lines().last().unwrap_or("")).unwrap();
    let actions = v["actions"].to_string();
    assert!(
        actions.contains("archived"),
        "done must report archival: {actions}"
    );

    assert!(
        archive_entry_exists(&state_dir, &merged),
        "archive entry must be written"
    );
    let after = harvestable(repo).join("\n");
    assert!(
        !after.contains(&merged),
        "merged row must leave harvestable:\n{after}"
    );

    // A Completed molecule without `merged_at` is still awaiting harvest.
    assert!(
        after.contains(&unmerged),
        "unmerged row must stay harvestable:\n{after}"
    );

    // Idempotent.
    let again = cs_isolated(repo)
        .args(["--json", "done", &merged, "--no-auto-propel"])
        .output()
        .unwrap();
    assert!(again.status.success());
}

/// `cs done <id> --if-completed` is the sweep form of the same harvest. On a
/// legacy merged row it must write the missing archive entry and set
/// `archived`, not stop at `already_merged`; it must stay a no-op for a
/// `Completed` row that was never merged, and be idempotent.
#[test]
fn cs_done_if_completed_archives_a_legacy_merged_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let state_dir = repo.join(".cosmon/state");

    let merged = nucleate_completed_no_branch(repo);
    let unmerged = nucleate_completed_no_branch(repo);
    make_legacy_merged(&state_dir, &merged, true);
    make_legacy_merged(&state_dir, &unmerged, false);

    let before = harvestable(repo).join("\n");
    assert!(
        before.contains(&merged),
        "precondition: legacy row is counted:\n{before}"
    );
    assert!(
        !archive_entry_exists(&state_dir, &merged),
        "precondition: no archive entry yet"
    );

    let done = cs_isolated(repo)
        .args([
            "--json",
            "done",
            &merged,
            "--if-completed",
            "--no-auto-propel",
        ])
        .output()
        .expect("cs done --if-completed");
    let stdout = String::from_utf8_lossy(&done.stdout);
    assert!(
        done.status.success(),
        "cs done --if-completed failed: {stdout}{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim().lines().last().unwrap_or("")).unwrap();
    assert_eq!(v["outcome"], "already_merged", "outcome label: {v}");
    assert!(
        v["actions"].to_string().contains("archived"),
        "--if-completed must report archival: {v}"
    );

    assert!(
        archive_entry_exists(&state_dir, &merged),
        "archive entry must be written"
    );
    let after = harvestable(repo).join("\n");
    assert!(
        !after.contains(&merged),
        "merged row must leave harvestable:\n{after}"
    );
    assert!(
        after.contains(&unmerged),
        "unmerged row must stay harvestable:\n{after}"
    );
    assert!(
        !archive_entry_exists(&state_dir, &unmerged),
        "unmerged row must not be archived"
    );

    // Idempotent: a second run succeeds and reports nothing new.
    let again = cs_isolated(repo)
        .args([
            "--json",
            "done",
            &merged,
            "--if-completed",
            "--no-auto-propel",
        ])
        .output()
        .unwrap();
    assert!(again.status.success());
    let v2: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(&again.stdout)
            .trim()
            .lines()
            .last()
            .unwrap_or(""),
    )
    .unwrap();
    assert!(
        !v2["actions"].to_string().contains("\"archived\""),
        "second run must not archive again: {v2}"
    );
}
