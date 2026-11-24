// SPDX-License-Identifier: AGPL-3.0-only

//! A `Completed` molecule tagged `no-auto-harvest` is landed only by an
//! explicit gesture (ADR-192 I9, one landing path per molecule; Refs #237).
//!
//! Every automatic path reaches `cs done --if-completed`, which answers
//! `reserved` and changes nothing for such a molecule. An explicit
//! `cs done <id>` still closes it, as does `cs harvest <id>` typed by the
//! operator. The molecules here have no branch, so "closed" is observable as
//! `archived` without a git merge in the way.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cs_isolated(repo: &Path) -> Command {
    let state_dir = repo.join(".cosmon/state");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .current_dir(&state_dir);
    cmd
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn failed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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
        "[project]\nproject_id = \"test-held-landing\"\n\n[archive]\nenabled = true\n",
    )
    .unwrap();
    let formula =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    git_ok(tmp, &["add", ".gitignore"]);
    git_ok(tmp, &["commit", "-q", "-m", "base"]);
}

/// A branchless `Completed` molecule, tagged when `tag` is given.
fn completed_molecule(repo: &Path, tag: Option<&str>) -> String {
    let mut args = vec!["--json", "nucleate", "task-work", "--var", "topic=held"];
    if let Some(tag) = tag {
        args.extend(["--tag", tag]);
    }
    let nuc = cs_isolated(repo).args(&args).output().expect("nucleate");
    assert!(nuc.status.success(), "{}", text(&nuc));
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let id = v["id"].as_str().expect("id").to_owned();
    let comp = cs_isolated(repo)
        .args([
            "--json",
            "complete",
            &id,
            "--reason",
            "held test",
            "--ignore-mindguard",
        ])
        .output()
        .expect("complete");
    assert!(comp.status.success(), "{}", text(&comp));
    id
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn is_archived(repo: &Path, id: &str) -> bool {
    let path = repo
        .join(".cosmon/state/fleets/default/molecules")
        .join(id)
        .join("state.json");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    v["archived"].as_bool().unwrap_or(false)
}

fn last_json(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim().lines().last().unwrap_or("")).unwrap_or_else(|e| {
        panic!("no JSON on stdout ({e}): {}", text(out));
    })
}

#[test]
fn done_if_completed_answers_reserved_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let id = completed_molecule(repo, Some("no-auto-harvest"));

    let out = cs_isolated(repo)
        .args(["--json", "done", &id, "--if-completed", "--no-auto-propel"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reserved is an outcome, not an error: {}",
        text(&out)
    );
    assert_eq!(last_json(&out)["outcome"], "reserved", "{}", text(&out));
    assert!(
        !is_archived(repo, &id),
        "a reserved molecule must stay open"
    );
}

#[test]
fn done_if_completed_still_closes_an_untagged_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let id = completed_molecule(repo, None);

    let out = cs_isolated(repo)
        .args(["--json", "done", &id, "--if-completed", "--no-auto-propel"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        is_archived(repo, &id),
        "an untagged molecule is closed: {}",
        text(&out)
    );
}

#[test]
fn explicit_done_closes_a_tagged_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let id = completed_molecule(repo, Some("no-auto-harvest"));

    let out = cs_isolated(repo)
        .args(["--json", "done", &id, "--no-auto-propel"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        is_archived(repo, &id),
        "explicit `cs done` is the gesture that lands: {}",
        text(&out)
    );
}

#[test]
fn pane_died_harvest_leaves_a_tagged_molecule_open() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let held = completed_molecule(repo, Some("no-auto-harvest"));
    let free = completed_molecule(repo, None);

    for id in [&held, &free] {
        let out = cs_isolated(repo)
            .args(["harvest", "--molecule", id, "--from-pane-died"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
    }
    assert!(
        is_archived(repo, &free),
        "control: the untagged molecule is harvested"
    );
    assert!(
        !is_archived(repo, &held),
        "pane-died harvest merged a tagged molecule"
    );
}

#[test]
fn patrol_harvest_leaves_a_tagged_molecule_open() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let held = completed_molecule(repo, Some("no-auto-harvest"));
    let free = completed_molecule(repo, None);

    let out = cs_isolated(repo)
        .args(["patrol", "--harvest"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        is_archived(repo, &free),
        "control: the untagged molecule is harvested"
    );
    assert!(
        !is_archived(repo, &held),
        "patrol --harvest merged a tagged molecule"
    );
}

#[test]
fn explicit_cs_harvest_still_closes_a_tagged_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let id = completed_molecule(repo, Some("no-auto-harvest"));

    let out = cs_isolated(repo)
        .args(["harvest", "--molecule", &id])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        is_archived(repo, &id),
        "an operator's own `cs harvest <id>` lands: {}",
        text(&out)
    );
}

/// The line `cs complete` prints for a molecule, in human mode.
fn complete_hint(repo: &Path, tag: Option<&str>) -> String {
    let mut args = vec!["--json", "nucleate", "task-work", "--var", "topic=hint"];
    if let Some(tag) = tag {
        args.extend(["--tag", tag]);
    }
    let nuc = cs_isolated(repo).args(&args).output().expect("nucleate");
    assert!(nuc.status.success(), "{}", text(&nuc));
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let id = v["id"].as_str().expect("id").to_owned();
    let out = cs_isolated(repo)
        .args(["complete", &id, "--reason", "hint", "--ignore-mindguard"])
        .output()
        .expect("complete");
    assert!(out.status.success(), "{}", text(&out));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn complete_says_a_tagged_molecule_lands_by_train_not_by_cs_done() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let free = complete_hint(repo, None);
    assert!(free.contains("to merge it"), "control: {free}");

    let held = complete_hint(repo, Some("no-auto-harvest"));
    assert!(
        held.contains("cs land-train"),
        "a tagged molecule's hint must say it lands by train: {held}"
    );
    assert!(
        !held.contains("to merge it"),
        "the plain \"run `cs done` to merge it\" hint is the wrong gesture here: {held}"
    );
}
