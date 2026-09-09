// SPDX-License-Identifier: AGPL-3.0-only

//! `cs done --dry-run` writes nothing durable — the falsifier for the PR
//! #62 review's fourth finding.
//!
//! THE HOLE IT CLOSES: the harvest reason is traced on the molecule
//! "before anything can fail", and that write sat *above* the `--dry-run`
//! branch. So `cs done <id> --dry-run --reason …` — the gesture an
//! operator uses to look at a plan before committing to it — changed the
//! molecule's durable record with a reason for a harvest that never
//! happened. A dry run promises no side effects, and a field a later
//! reader will treat as the account of record is a side effect whatever
//! else the run avoided.
//!
//! Asserted against the molecule's own `state.json` before and after,
//! through the **real `cs` binary**, because the claim is about what is on
//! disk when the process exits.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cs_isolated(repo: &Path) -> Command {
    let state_dir = repo.join(".cosmon/state");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env("COSMON_ASSUME_TRUSTED", "1")
        .current_dir(&state_dir);
    cmd
}

fn git_ok(repo: &Path, args: &[&str]) {
    let mut full: Vec<&str> = vec!["-C", repo.to_str().unwrap()];
    full.extend_from_slice(args);
    let out = Command::new("git").args(&full).output().expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git repo with a `.cosmon` project and the `task-work` formula — the
/// minimum `cs nucleate` needs.
fn setup_repo(repo: &Path) {
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "Test"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);

    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        "[project]\nproject_id = \"test-done-dry-run\"\n",
    )
    .unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);
}

/// Nucleate and collapse: a terminal molecule `cs done` will accept
/// without `--force`.
fn nucleate_terminal(repo: &Path) -> String {
    let nuc = cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=dry run leaves no trace",
        ])
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let id = v["id"].as_str().expect("nucleate id").to_owned();

    let col = cs_isolated(repo)
        .args(["--json", "collapse", &id, "--reason", "dry-run fixture"])
        .output()
        .expect("cs collapse");
    assert!(
        col.status.success(),
        "collapse failed: {}",
        String::from_utf8_lossy(&col.stderr)
    );
    id
}

/// The molecule's own record on disk. Compared whole rather than
/// field-by-field: the claim is "no side effects", and a test that only
/// looked at `harvest_reason` would pass while a dry run stamped
/// something else.
fn state_file(repo: &Path, id: &str) -> PathBuf {
    let fleets = repo.join(".cosmon/state/fleets");
    for fleet in fs::read_dir(&fleets).expect("fleets dir") {
        let candidate = fleet
            .expect("fleet entry")
            .path()
            .join("molecules")
            .join(id)
            .join("state.json");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!("no state.json for {id} under {}", fleets.display());
}

#[test]
fn a_dry_run_with_a_reason_does_not_record_that_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = nucleate_terminal(repo);

    let path = state_file(repo, &mol_id);
    let before = fs::read_to_string(&path).expect("read state before");
    assert!(
        !before.contains("a reason the operator was only trying out"),
        "fixture premise: the reason is not on the molecule yet"
    );

    let out = cs_isolated(repo)
        .args([
            "done",
            &mol_id,
            "--dry-run",
            "--reason",
            "a reason the operator was only trying out",
        ])
        .output()
        .expect("cs done --dry-run");
    assert!(
        out.status.success(),
        "a dry run must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = fs::read_to_string(&path).expect("read state after");
    assert_eq!(
        before, after,
        "`--dry-run` promises no side effects; the molecule's durable record changed"
    );
}

/// The other half of the contract, so the fix above cannot be "never
/// write the reason": a real `cs done` still records it. Both halves in
/// one file because a test that only proved the absence would be passed by
/// deleting the feature.
#[test]
fn a_real_run_still_records_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = nucleate_terminal(repo);

    let out = cs_isolated(repo)
        .args([
            "done",
            &mol_id,
            "--reason",
            "the reason of record for this closure",
            "--no-auto-propel",
        ])
        .output()
        .expect("cs done");
    // The teardown may or may not find a branch to merge in this fixture;
    // what is asserted is the trace, which is written before any of that.
    let state = fs::read_to_string(state_file(repo, &mol_id)).expect("read state");
    assert!(
        state.contains("the reason of record for this closure"),
        "a real harvest traces its reason.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
