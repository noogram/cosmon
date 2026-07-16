// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI coverage for the `cs done` merge-conflict silent-integrity
//! fix.
//!
//! THE BUG: a feat branch whose files also changed on main produced a merge
//! conflict, yet `cs done` printed *"done with 1 warning"* and returned — the
//! work never merged, but the word "done" + an easy-to-miss warning made it
//! look like it had. An operator would ship a stale artifact.
//!
//! THE CONTRACT exercised here, against the **real `cs` binary** so the exit
//! code is the actual process exit (no in-process `run()` cwd mutation, which
//! is unsafe in a parallel test binary):
//!
//!   * conflict ⇒ **non-zero exit**, a loud `merge_conflict` outcome, the
//!     branch preserved, and nothing landed on main;
//!   * a clean merge ⇒ **zero exit** and teardown proceeds (branch deleted).
//!
//! The hermetic unit tests in `cmd::done` cover the merge-decision layer
//! (`try_merge_with_escalation`); this file proves the command-level wiring.

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
        // cwd inside the repo → `git rev-parse --show-toplevel` returns `repo`.
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

/// Init a git repo with a `.cosmon` project whose state is **gitignored** —
/// so `cs done`'s pre-merge state flush has nothing tracked to commit, and the
/// only thing that can move `main` is the merge itself.
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
        "[project]\nproject_id = \"test-done-conflict\"\n",
    )
    .unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    // Keep all cosmon state + worktrees out of git history.
    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
}

/// Nucleate a `task-work` molecule and drive it to a terminal state
/// (collapsed) so `cs done` will attempt the merge without `--force`.
fn nucleate_terminal(repo: &Path) -> String {
    let nuc = cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=done-conflict integration test",
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

    let col = cs_isolated(repo)
        .args([
            "--json",
            "collapse",
            &mol_id,
            "--reason",
            "integration test",
        ])
        .output()
        .expect("cs collapse");
    assert!(
        col.status.success(),
        "collapse failed: {}",
        String::from_utf8_lossy(&col.stderr)
    );
    mol_id
}

#[test]
fn cs_done_fails_loudly_on_merge_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let mol_id = nucleate_terminal(repo);
    let branch = format!("feat/{mol_id}");

    // Base commit carries a shared file both sides will edit.
    fs::write(repo.join("shared.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "shared.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);

    // Worker branch edits shared.txt.
    git_ok(repo, &["checkout", "-q", "-b", &branch]);
    fs::write(repo.join("shared.txt"), "from worker\n").unwrap();
    git_ok(repo, &["commit", "-qam", "worker edit"]);

    // main edits the same line — guarantees a textual conflict.
    git_ok(repo, &["checkout", "-q", "main"]);
    fs::write(repo.join("shared.txt"), "from main\n").unwrap();
    git_ok(repo, &["commit", "-qam", "main edit"]);

    let main_before = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();

    // The harvest. `--no-auto-propel` so the conflict surfaces immediately
    // (no 30s escalation backoff).
    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");

    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. NON-ZERO EXIT — the heart of the fix. Pre-fix this was exit 0.
    assert!(
        !done.status.success(),
        "cs done MUST fail on a merge conflict (got success).\nstdout={stdout}\nstderr={stderr}"
    );

    // 2. Loud, typed conflict outcome — never "done with 1 warning".
    assert!(
        stdout.contains("merge_conflict") || stderr.contains("MERGE CONFLICT"),
        "expected a loud merge_conflict signal.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("\"ok\":true"),
        "JSON must not report ok:true on a conflict.\nstdout={stdout}"
    );

    // 3. Branch preserved — teardown did not delete the only copy of the work.
    let show_branch = git(repo, &["rev-parse", "--verify", &branch]);
    assert!(
        show_branch.status.success(),
        "the worker's branch must survive a failed merge"
    );

    // 4. Nothing landed on main: no merge commit, shared.txt still main's.
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_before, main_after,
        "main HEAD must NOT move on a merge conflict (no merge commit)"
    );
    let main_shared = String::from_utf8_lossy(&git(repo, &["show", "main:shared.txt"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_shared, "from main",
        "the worker's content must NOT have landed on main"
    );

    // 5. Worktree clean — the conflict was rolled back (no MERGE_HEAD).
    assert!(
        !repo.join(".git/MERGE_HEAD").exists(),
        "working tree must be clean after a rolled-back conflict"
    );
}

#[test]
fn cs_done_clean_merge_tears_down() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let mol_id = nucleate_terminal(repo);
    let branch = format!("feat/{mol_id}");

    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);

    // Disjoint edits → clean 3-way merge.
    git_ok(repo, &["checkout", "-q", "-b", &branch]);
    fs::write(repo.join("worker.txt"), "worker\n").unwrap();
    git_ok(repo, &["add", "worker.txt"]);
    git_ok(repo, &["commit", "-qm", "worker file"]);

    git_ok(repo, &["checkout", "-q", "main"]);
    fs::write(repo.join("main.txt"), "main\n").unwrap();
    git_ok(repo, &["add", "main.txt"]);
    git_ok(repo, &["commit", "-qm", "main advance"]);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");

    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. ZERO EXIT — the happy path must not regress.
    assert!(
        done.status.success(),
        "clean merge must succeed.\nstdout={stdout}\nstderr={stderr}"
    );

    // 2. Merge landed: the worker's file is now on main.
    let worker_on_main = git(repo, &["cat-file", "-e", "main:worker.txt"]);
    assert!(
        worker_on_main.status.success(),
        "worker.txt must be present on main after a clean merge"
    );

    // 3. Teardown proceeded: the feat branch was deleted.
    let branch_gone = git(repo, &["rev-parse", "--verify", &branch]);
    assert!(
        !branch_gone.status.success(),
        "a clean merge must delete the feat branch (teardown proceeds)"
    );
}
