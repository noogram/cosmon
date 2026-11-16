// SPDX-License-Identifier: AGPL-3.0-only

//! `cs done --print-merge-message` exports the completion merge message
//! (issue #213).
//!
//! An operator who builds a harvest train outside cosmon must be able to write
//! the same merge commit message `cs done` would have written. The contract,
//! through the real `cs` binary: the printed text changes no state, and
//! `git merge --no-ff -F <printed>` in one fixture yields a commit message
//! byte-identical to the one `cs done` writes in an identical fixture.
//!
//! The comparison is on the stored messages of repositories with no
//! `commit-msg`, `prepare-commit-msg` or sign-off hook, i.e. before any such
//! hook acts: a hook rewrites whatever message it is given, so it says nothing
//! about whether the two renderings agree.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cs_isolated(repo: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_BASE_BRANCH")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .current_dir(repo.join(".cosmon/state"));
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .output()
        .expect("git spawn failed")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev(repo: &Path, refname: &str) -> String {
    String::from_utf8_lossy(&git(repo, &["rev-parse", refname]).stdout)
        .trim()
        .to_owned()
}

const CONFIG: &str = "[project]\nproject_id = \"test-merge-message\"\n\
     merge_subject = \"merge {mol_id}: {title}\"\n\n\
     [attribution]\npublic_name = \"Noogram\"\npublic_url = \"noogram.org\"\n\
     coauthor_email = \"noreply@noogram.org\"\n";

fn setup_repo(repo: &Path) {
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "Test"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);

    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(cosmon.join("config.toml"), CONFIG).unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    fs::write(repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);
}

fn completed_molecule(repo: &Path) -> String {
    let out = cs_isolated(repo)
        .args(["--json", "nucleate", "task-work", "--var", "topic=export the message"])
        .output()
        .expect("cs nucleate");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();
    let done = cs_isolated(repo)
        .args(["complete", &mol_id, "--reason", "test", "--ignore-mindguard"])
        .output()
        .expect("cs complete");
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    git_ok(repo, &["checkout", "-q", "-b", &format!("feat/{mol_id}"), "main"]);
    fs::write(repo.join("worker.txt"), "worker\n").unwrap();
    git_ok(repo, &["add", "worker.txt"]);
    git_ok(repo, &["commit", "-qm", "feat: worker deliverable"]);
    git_ok(repo, &["checkout", "-q", "main"]);
    mol_id
}

fn copy_dir(from: &Path, to: &Path) {
    let status = Command::new("cp")
        .arg("-R")
        .arg(from)
        .arg(to)
        .status()
        .expect("cp");
    assert!(status.success());
}

fn state_bytes(repo: &Path, mol_id: &str) -> Vec<u8> {
    fs::read(
        repo.join(".cosmon/state/fleets/default/molecules")
            .join(mol_id)
            .join("state.json"),
    )
    .unwrap()
}

fn head_message(repo: &Path) -> String {
    String::from_utf8_lossy(&git(repo, &["log", "-1", "--format=%B", "main"]).stdout).into_owned()
}

/// Printing changes nothing: no merge, no stamp, no state byte.
#[test]
fn print_merge_message_changes_no_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    let main_before = rev(repo, "main");
    let state_before = state_bytes(repo, &mol_id);

    let out = cs_isolated(repo)
        .args(["done", &mol_id, "--print-merge-message"])
        .output()
        .expect("cs done --print-merge-message");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.starts_with(&format!("merge {mol_id}: export the message\n\n")),
        "subject from [project] merge_subject, then a blank line: {printed:?}"
    );
    assert!(printed.contains("Co-Authored-By: Noogram"), "{printed}");
    assert!(printed.contains(&format!("Mol-Id: {mol_id}")), "{printed}");
    assert_eq!(main_before, rev(repo, "main"), "no merge happened");
    assert_eq!(state_before, state_bytes(repo, &mol_id), "no state change");
}

/// The printed message, merged by hand, equals what `cs done` writes.
#[test]
fn printed_message_is_byte_identical_to_the_one_cs_done_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir(&a).unwrap();
    setup_repo(&a);
    let mol_id = completed_molecule(&a);
    copy_dir(&a, &b);

    let printed = cs_isolated(&a)
        .args(["done", &mol_id, "--print-merge-message"])
        .output()
        .expect("print");
    assert!(printed.status.success(), "{}", String::from_utf8_lossy(&printed.stderr));
    let file = tmp.path().join("message.txt");
    fs::write(&file, &printed.stdout).unwrap();

    git_ok(
        &a,
        &[
            "merge",
            "--no-ff",
            "-q",
            "-F",
            file.to_str().unwrap(),
            &format!("feat/{mol_id}"),
        ],
    );

    let merged = cs_isolated(&b)
        .args(["done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    assert!(merged.status.success(), "{}", String::from_utf8_lossy(&merged.stderr));

    let by_hand = head_message(&a);
    let by_cs = head_message(&b);
    assert!(by_cs.contains(&format!("Mol-Id: {mol_id}")), "{by_cs}");
    assert_eq!(by_hand, by_cs, "the two merge messages must be byte-identical");
}
