// SPDX-License-Identifier: AGPL-3.0-only

//! Protected reference inputs survive the harvest (issue #94).
//!
//! A worker handed reference data to validate a fix against proposed to
//! replace the reference with its own output because the two were "very
//! close". Nothing at `cs done` would have noticed. These tests run the real
//! `cs` binary and pin the contract that closes that gap:
//!
//! * `cs nucleate --protect <path>` persists the path on the molecule;
//! * a worker branch that changes a protected path is refused by `cs done`
//!   with exit code 78, the path named, and nothing merged;
//! * the same branch, once the protected path is restored, merges;
//! * the operator can override the refusal explicitly;
//! * a path that could never match (absolute, `..`) is refused at birth.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const REFERENCE: &str = "ref/expected.csv";

fn cs_isolated(repo: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_BASE_BRANCH")
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

/// A repository whose `main` carries a reference dataset under `ref/`.
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
        "[project]\nproject_id = \"test-protected-paths\"\n",
    )
    .unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    fs::write(repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    fs::create_dir_all(repo.join("ref")).unwrap();
    fs::write(repo.join(REFERENCE), "x,y\n1,2.000\n").unwrap();
    git_ok(repo, &["add", ".gitignore", REFERENCE]);
    git_ok(repo, &["commit", "-q", "-m", "base with reference data"]);
}

/// Nucleate a `task-work` molecule with the given extra arguments.
fn nucleate(repo: &Path, extra: &[&str]) -> Output {
    cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=validate a fix",
        ])
        .args(extra)
        .output()
        .expect("cs nucleate")
}

/// Nucleate a molecule, optionally protecting one path, and make it terminal.
fn terminal_molecule(repo: &Path, protected: Option<&str>) -> String {
    let extra = protected.map_or_else(Vec::new, |path| vec!["--protect", path]);
    let out = nucleate(repo, &extra);
    assert!(
        out.status.success(),
        "nucleate --protect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();

    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(
            repo.join(".cosmon/state/fleets/default/molecules")
                .join(&mol_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    if let Some(path) = protected {
        let expected = path
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_owned();
        assert_eq!(state["protected_paths"], serde_json::json!([expected]));
    } else {
        assert!(
            state["protected_paths"].is_null(),
            "an undeclared protection remains absent from the persisted JSON"
        );
    }

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
    assert!(col.status.success(), "collapse failed");
    mol_id
}

/// Nucleate a molecule protecting `ref/` and bring it to a terminal state.
fn protected_molecule(repo: &Path) -> String {
    terminal_molecule(repo, Some("./ref/"))
}

/// The worker's branch: a legitimate change plus, when `overwrite_reference`,
/// the reference replaced by the worker's own output.
fn worker_branch(repo: &Path, mol_id: &str, overwrite_reference: bool) {
    git_ok(
        repo,
        &["checkout", "-q", "-b", &format!("feat/{mol_id}"), "main"],
    );
    fs::write(repo.join("fix.txt"), "the fix\n").unwrap();
    git_ok(repo, &["add", "fix.txt"]);
    git_ok(repo, &["commit", "-qm", "fix"]);
    if overwrite_reference {
        fs::write(repo.join(REFERENCE), "x,y\n1,2.003\n").unwrap();
        git_ok(
            repo,
            &["commit", "-qam", "reference is very close, use ours"],
        );
    }
    git_ok(repo, &["checkout", "-q", "main"]);
}

fn done(repo: &Path, mol_id: &str, extra: &[&str]) -> Output {
    cs_isolated(repo)
        .args(["done", mol_id, "--no-auto-propel"])
        .args(extra)
        .output()
        .expect("cs done")
}

/// The issue's reproduction: the branch that rewrites the reference is
/// refused, and the same branch without that change merges.
#[test]
fn done_refuses_a_branch_that_modifies_a_protected_path() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = protected_molecule(repo);
    worker_branch(repo, &mol_id, true);
    let main_before = rev(repo, "main");

    let refused = done(repo, &mol_id, &[]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(
        refused.status.code(),
        Some(78),
        "a branch changing a protected path must be refused with exit 78.\nstderr={stderr}"
    );
    assert!(
        stderr.contains("protected_path_modified") && stderr.contains(REFERENCE),
        "the refusal must carry its label and name the path: {stderr}"
    );
    assert_eq!(main_before, rev(repo, "main"), "nothing may merge");
    assert!(
        git(repo, &["rev-parse", "--verify", &format!("feat/{mol_id}")])
            .status
            .success(),
        "the branch must survive the refusal"
    );

    // Restore the reference on the same branch: now it merges.
    let branch = format!("feat/{mol_id}");
    git_ok(repo, &["checkout", "-q", &branch]);
    git_ok(repo, &["checkout", "main", "--", REFERENCE]);
    git_ok(repo, &["commit", "-qm", "restore the reference"]);
    git_ok(repo, &["checkout", "-q", "main"]);

    let merged = done(repo, &mol_id, &[]);
    assert!(
        merged.status.success(),
        "the same branch without the protected change must merge: {}",
        String::from_utf8_lossy(&merged.stderr)
    );
    assert!(git(repo, &["cat-file", "-e", "main:fix.txt"])
        .status
        .success());
    assert_eq!(
        String::from_utf8_lossy(&git(repo, &["show", &format!("main:{REFERENCE}")]).stdout),
        "x,y\n1,2.000\n",
        "the reference on main must be the original"
    );
}

/// A branch that leaves the protected path alone merges on the first try.
#[test]
fn done_merges_a_branch_that_leaves_protected_paths_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = protected_molecule(repo);
    worker_branch(repo, &mol_id, false);

    let merged = done(repo, &mol_id, &[]);
    assert!(
        merged.status.success(),
        "an untouched reference must not block the merge: {}",
        String::from_utf8_lossy(&merged.stderr)
    );
    assert!(git(repo, &["cat-file", "-e", "main:fix.txt"])
        .status
        .success());
}

/// The operator can land an intended change to the reference, explicitly.
#[test]
fn allow_protected_change_lets_the_operator_override() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = protected_molecule(repo);
    worker_branch(repo, &mol_id, true);

    let merged = done(repo, &mol_id, &["--allow-protected-change"]);
    assert!(
        merged.status.success(),
        "--allow-protected-change must lift the refusal: {}",
        String::from_utf8_lossy(&merged.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&git(repo, &["show", &format!("main:{REFERENCE}")]).stdout),
        "x,y\n1,2.003\n"
    );
}

/// A protected path that can never match is refused before a molecule exists.
#[test]
fn nucleate_refuses_a_protected_path_outside_the_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    for bad in ["/etc/reference.csv", "../reference.csv"] {
        let out = nucleate(repo, &["--protect", bad]);
        assert!(!out.status.success(), "--protect {bad} must be refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--protect"),
            "the refusal must name the flag"
        );
    }
    assert!(
        !repo.join(".cosmon/state/fleets/default/molecules").exists()
            || fs::read_dir(repo.join(".cosmon/state/fleets/default/molecules"))
                .unwrap()
                .next()
                .is_none(),
        "no molecule may be created for a refused --protect"
    );
}

/// NUL-delimited Git output preserves every byte of unusual UTF-8 names.
#[cfg(unix)]
#[test]
fn done_refuses_exact_protected_paths_with_git_quoted_characters() {
    for path in [
        "ref/référence.csv",
        "ref/a \"quoted\" reference.csv",
        "ref/a reference\nwith two lines.csv",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_repo(repo);
        fs::write(repo.join(path), "original\n").unwrap();
        git_ok(repo, &["add", path]);
        git_ok(repo, &["commit", "-qm", "add unusual reference name"]);

        let mol_id = terminal_molecule(repo, Some(path));
        git_ok(
            repo,
            &["checkout", "-q", "-b", &format!("feat/{mol_id}"), "main"],
        );
        fs::write(repo.join(path), "worker output\n").unwrap();
        git_ok(repo, &["commit", "-qam", "rewrite unusual reference"]);
        git_ok(repo, &["checkout", "-q", "main"]);

        let refused = done(repo, &mol_id, &[]);
        assert_eq!(
            refused.status.code(),
            Some(78),
            "{path:?} must be detected through the real command: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains(path),
            "the refusal must name {path:?}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
}

/// Renaming a protected file changes its old path and must be refused.
#[test]
fn done_refuses_renaming_a_protected_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = terminal_molecule(repo, Some(REFERENCE));
    git_ok(
        repo,
        &["checkout", "-q", "-b", &format!("feat/{mol_id}"), "main"],
    );
    git_ok(repo, &["mv", REFERENCE, "renamed-reference.csv"]);
    git_ok(repo, &["commit", "-qm", "rename protected reference"]);
    git_ok(repo, &["checkout", "-q", "main"]);

    let refused = done(repo, &mol_id, &[]);
    assert_eq!(
        refused.status.code(),
        Some(78),
        "renaming the protected file must be refused: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// Molecules with no protection retain the pre-protection harvest behaviour.
#[test]
fn done_allows_changes_when_the_molecule_declared_no_protection() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = terminal_molecule(repo, None);
    worker_branch(repo, &mol_id, true);

    let merged = done(repo, &mol_id, &[]);
    assert!(
        merged.status.success(),
        "an unprotected molecule must remain unrestricted: {}",
        String::from_utf8_lossy(&merged.stderr)
    );
}

/// Batch declarations cannot silently discard a command-line protection.
#[test]
fn nucleate_refuses_protect_with_from_before_creating_a_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let declarations = repo.join("declarations");
    fs::create_dir_all(&declarations).unwrap();
    fs::write(
        declarations.join("one.toml"),
        "id_prefix = \"batch\"\nformula = \"task-work\"\ndescription = \"one\"\n\n[variables]\ntopic = \"one\"\n",
    )
    .unwrap();

    let out = cs_isolated(repo)
        .args(["nucleate", "--from"])
        .arg(&declarations)
        .args(["--protect", REFERENCE])
        .output()
        .expect("cs nucleate --from --protect");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "--protect must not be accepted and discarded with --from"
    );
    assert!(
        stderr.contains("--protect") && stderr.contains("--from"),
        "the refusal must name both incompatible flags: {stderr}"
    );
    assert!(
        !repo.join(".cosmon/state/fleets/default/molecules").exists()
            || fs::read_dir(repo.join(".cosmon/state/fleets/default/molecules"))
                .unwrap()
                .next()
                .is_none(),
        "the refused combination must not create a molecule"
    );
}
