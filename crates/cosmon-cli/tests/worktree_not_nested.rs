// SPDX-License-Identifier: AGPL-3.0-only

//! A worker's worktree is sited under the **galaxy**, never under another
//! worker's worktree.
//!
//! # What was observed
//!
//! On 2026-08-08, five worktrees had been created one inside another:
//!
//! ```text
//! .worktrees/converge-20260802-0b09/.worktrees/cmbverify-1ce6
//! .worktrees/task-20260807-1921/.worktrees/verify-20260807-9c52
//! ```
//!
//! The nesting is not a cosmetic sin. `cs done` on the *parent* removes the
//! parent's directory and takes the child's with it; git keeps the child's
//! registration, still counts `feat/<child>` as checked out, and the child's
//! own `cs done` fails with `cannot delete branch … used by worktree`. What
//! it leaves behind — a ghost branch and a dangling registration — had to be
//! swept by hand with `git worktree remove --force` and `git worktree prune`.
//!
//! # The cause
//!
//! `cs tackle` sites a worktree at `<repo_root>/.worktrees/<mol>`, and
//! `repo_root` came from `git rev-parse --show-toplevel` run in the current
//! directory. Inside `…/<galaxy>/.worktrees/task-A`, that command answers
//! with the *worktree* — which is the true answer to the question git was
//! asked, and the wrong answer to the question cosmon meant. A worker
//! tackling a child from its own worktree therefore rooted `.worktrees/`
//! at itself.
//!
//! # What this file pins
//!
//! Through the real `cs` binary: a molecule tackled **from inside another
//! molecule's worktree** is told a sandbox root directly under the galaxy,
//! and specifically not one containing a second `.worktrees` component.
//!
//! `--dry-run` is the probe because the property is decided before any pane
//! or model call, and because the dry run prints the very path a real
//! dispatch would create — the unit test
//! `predicted_sandbox_root_mirrors_the_dispatch_expression` in `tackle.rs`
//! is what keeps the printed path and the created path the same expression.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A `cs` invocation with the ambient cosmon session stripped, so the run is
/// hermetic: the suite is itself executed by a worker whose environment
/// carries a depth, an adapter and a model that would otherwise steer it.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_DEFAULT_MODEL",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
        "ANTHROPIC_MODEL",
        "COSMON_EGRESS_POLICY",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A galaxy that is also a git repository with one commit, carrying a
/// formula any adapter can run (the capability gate is a different subject).
fn galaxy(tmp: &Path) -> PathBuf {
    // Canonicalized: on macOS a temp dir is reached through `/var` while git
    // reports `/private/var`, and a test comparing the two strings would fail
    // for a reason that has nothing to do with worktrees.
    let root = tmp.canonicalize().unwrap().join("galaxy");
    fs::create_dir_all(&root).unwrap();
    assert!(cs(&root).arg("init").status().unwrap().success());

    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "Test"]);
    fs::write(root.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "seed"]);

    let formulas = root.join(".cosmon").join("formulas");
    fs::create_dir_all(&formulas).unwrap();
    fs::write(
        formulas.join("note-work.formula.toml"),
        r#"formula = "note-work"
version = 1
description = "A mission any worker can satisfy."
id_prefix = "task"

[vars.topic]
description = "The task."
required = true

[[steps]]
id = "answer"
title = "Answer"
description = "Write the answer to result.md."
"#,
    )
    .unwrap();
    root
}

/// Nucleate a `note-work` molecule and return its id.
fn nucleate(root: &Path) -> String {
    let out = cs(root)
        .args([
            "--json",
            "nucleate",
            "note-work",
            "--kind",
            "task",
            "--var",
            "topic=a mission",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("`cs nucleate --json` must emit json")
        .get("id")
        .and_then(|v| v.as_str())
        .expect("`cs nucleate --json` must carry the molecule id")
        .to_owned()
}

/// The sandbox root `cs tackle` announces for `mol` when fired from `cwd`.
///
/// The local adapter's brief is the one that states the root outright,
/// because a confined worker is told the only directory it may write to.
fn announced_sandbox_root(cwd: &Path, mol: &str) -> String {
    let out = cs(cwd)
        .args(["--json", "tackle", mol, "--adapter", "local", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "tackle --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    v.get("prompt")
        .and_then(|p| p.as_str())
        .expect("the dry run must carry the prompt")
        .to_owned()
}

#[test]
fn a_child_tackled_from_a_worktree_is_not_nested_inside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());

    let parent = nucleate(&root);
    let child = nucleate(&root);

    // The parent's worktree, sited exactly as `cs tackle` sites one.
    let parent_wt = root.join(".worktrees").join(&parent);
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            &format!("feat/{parent}"),
            &parent_wt.to_string_lossy(),
            "main",
        ],
    );
    assert!(parent_wt.is_dir(), "fixture must produce a real worktree");

    // The dispatch under test: a worker inside the parent's worktree
    // tackling the child, which is how every observed nesting was born.
    let prompt = announced_sandbox_root(&parent_wt, &child);

    let expected = root.join(".worktrees").join(&child);
    let nested = parent_wt.join(".worktrees").join(&child);
    assert!(
        !prompt.contains(&*nested.to_string_lossy()),
        "the child must not be sited inside the parent's worktree.\n\
         nested path: {}\nprompt:\n{prompt}",
        nested.display()
    );
    assert!(
        prompt.contains(&*expected.to_string_lossy()),
        "the child must be sited under the galaxy.\n\
         expected: {}\nprompt:\n{prompt}",
        expected.display()
    );
}

#[test]
fn tackling_from_the_galaxy_root_is_unchanged() {
    // The redirection must be invisible to the ordinary case; a fix that
    // only holds inside a worktree, and moves the normal dispatch, is not a
    // fix.
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let mol = nucleate(&root);

    let prompt = announced_sandbox_root(&root, &mol);
    let expected = root.join(".worktrees").join(&mol);
    assert!(
        prompt.contains(&*expected.to_string_lossy()),
        "expected: {}\nprompt:\n{prompt}",
        expected.display()
    );
}
