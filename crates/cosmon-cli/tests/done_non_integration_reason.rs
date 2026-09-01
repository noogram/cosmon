// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI coverage for the **persisted non-integration reason**
//! (C6 of delib-20260819-cda2, issue #51 third defect).
//!
//! THE BUG: `status == "completed"` did not distinguish "merged and
//! archived" from "conflict, branch stranded". The triplet
//! `(Completed, merged_at = None, archived)` carried at least four
//! disjoint meanings — never harvested, conflict, refused `pre_done`
//! gate, molecule without a branch — and only two of them left a
//! `MergeCompleted` event in the journal. So there was no retry
//! predicate: every drain re-attempted the same blocked molecules.
//!
//! THE CONTRACT exercised here, against the **real `cs` binary**:
//!
//!   * a `no_branch` teardown writes `non_integration.reason =
//!     "no-branch"` next to `merged_at`, and archives;
//!   * `cs done --no-merge` — which used to archive nothing at all, and
//!     so manufactured a permanent `CompletedUnharvested` anomaly —
//!     archives WITH `reason = "merge-skipped"`, and preserves the branch;
//!   * a molecule that is merely un-harvested carries NO reason: the
//!     absence of the field is the fourth meaning, and inventing a reason
//!     for it would fabricate a refusal that never happened;
//!   * the persisted spelling is the same kebab-case string the API
//!     serves, so `jq` over `state.json` and the tenant agree.
//!
//! The two remaining reasons (`conflict`, `pre-done-refused`) are covered
//! by the unit tests on their write paths; staging a real textual conflict
//! and a real hook refusal end-to-end belongs to
//! `done_pre_done_gate.rs`'s fixture machinery, not here.

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

/// Init a git repo with a `.cosmon` project whose archive subsystem is
/// enabled — the gate every terminal teardown flows through.
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
        "[project]\nproject_id = \"test-non-integration-reason\"\n\n[archive]\nenabled = true\n",
    )
    .unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    git_ok(tmp, &["add", ".gitignore"]);
    git_ok(tmp, &["commit", "-q", "-m", "base"]);
}

/// Nucleate a `task-work` molecule and drive it to `Completed`.
fn nucleate_completed(repo: &Path, topic: &str) -> String {
    let nuc = cs_isolated(repo)
        .args(["--json", "nucleate", "task-work", "--var", topic])
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();

    let comp = cs_isolated(repo)
        .args([
            "--json",
            "complete",
            &mol_id,
            "--reason",
            "non-integration reason test",
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

/// Read the molecule's persisted `state.json`.
fn load_state(state_dir: &Path, mol_id: &str) -> serde_json::Value {
    let path = state_dir
        .join("fleets/default/molecules")
        .join(mol_id)
        .join("state.json");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).expect("state.json is JSON")
}

/// A completed molecule that was never harvested records NO reason. The
/// absence of the field IS the fourth meaning; a default reason here would
/// fabricate a refusal that never happened.
#[test]
fn an_unharvested_molecule_records_no_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let state_dir = repo.join(".cosmon/state");

    let mol_id = nucleate_completed(repo, "topic=never harvested");
    let state = load_state(&state_dir, &mol_id);

    assert_eq!(state["status"], serde_json::json!("completed"));
    assert!(
        state.get("merged_at").is_none() || state["merged_at"].is_null(),
        "precondition: not merged"
    );
    assert!(
        state.get("non_integration").is_none() || state["non_integration"].is_null(),
        "an un-harvested molecule must carry no reason, got {:?}",
        state.get("non_integration")
    );
}

/// A `no_branch` teardown is terminal but NOT integrated — and now says so.
/// Before C6 it left a bare `(Completed, merged_at = null, archived)` for
/// the reader to guess at.
#[test]
fn no_branch_teardown_records_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let state_dir = repo.join(".cosmon/state");

    let mol_id = nucleate_completed(repo, "topic=no branch");
    let branch = format!("feat/{mol_id}");
    assert!(
        !git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "precondition: no feat branch"
    );

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    assert!(
        done.status.success(),
        "cs done must succeed on a no_branch molecule: {}",
        String::from_utf8_lossy(&done.stderr)
    );

    let state = load_state(&state_dir, &mol_id);
    assert_eq!(
        state["non_integration"]["reason"],
        serde_json::json!("no-branch"),
        "state was {state:#}"
    );
    assert_eq!(
        state["non_integration"]["base_branch"],
        serde_json::json!("main"),
        "the reason must name the trunk it is relative to"
    );
    assert_eq!(state["archived"], serde_json::json!(true));
    assert!(
        state.get("merged_at").is_none() || state["merged_at"].is_null(),
        "merged_at and non_integration are complements — never both set"
    );
}

/// THE `--no-merge` FIX. The flag used to reach the end of `cs done` having
/// stamped neither `merged_at` nor `archived`, manufacturing a
/// `CompletedUnharvested` anomaly (A8) that the health pass re-flagged on
/// every sweep. A deliberate choice is now written down as a state.
#[test]
fn no_merge_archives_with_the_merge_skipped_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let state_dir = repo.join(".cosmon/state");

    let mol_id = nucleate_completed(repo, "topic=no merge");
    let branch = format!("feat/{mol_id}");

    // Give it a real feat branch carrying a commit, so `--no-merge` is a
    // genuine refusal to integrate work rather than an empty no-op.
    git_ok(repo, &["checkout", "-q", "-b", &branch]);
    fs::write(repo.join("deliverable.txt"), "worker output\n").unwrap();
    git_ok(repo, &["add", "deliverable.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "worker work"]);
    git_ok(repo, &["checkout", "-q", "main"]);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-merge", "--no-auto-propel"])
        .output()
        .expect("cs done --no-merge");
    assert!(
        done.status.success(),
        "cs done --no-merge must succeed: {}",
        String::from_utf8_lossy(&done.stderr)
    );

    let state = load_state(&state_dir, &mol_id);
    assert_eq!(
        state["non_integration"]["reason"],
        serde_json::json!("merge-skipped"),
        "state was {state:#}"
    );
    assert_eq!(
        state["archived"],
        serde_json::json!(true),
        "--no-merge is a terminal teardown: it must archive, or A8 re-flags \
         it forever. state was {state:#}"
    );
    assert!(
        state.get("merged_at").is_none() || state["merged_at"].is_null(),
        "nothing landed, so nothing may be stamped"
    );

    // The branch is preserved — `--no-merge` refuses integration, it does
    // not discard the only copy of the work.
    assert!(
        git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "--no-merge must preserve the feat branch"
    );
}
