// SPDX-License-Identifier: AGPL-3.0-only

//! Harvest refuses overlapping local edits before invoking `git merge`.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Galaxy {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    shim: PathBuf,
    trace: PathBuf,
    id: String,
    worktree: PathBuf,
}

impl Galaxy {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("galaxy");
        let shim = tmp.path().join("bin");
        let trace = tmp.path().join("merges");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&shim).unwrap();
        let repo = fs::canonicalize(repo).unwrap();
        let real_git = Command::new("which").arg("git").output().unwrap();
        let real_git = String::from_utf8(real_git.stdout).unwrap();
        // Log the actual merge effect, not merge-base/merge-tree observations.
        fs::write(
            shim.join("git"),
            format!(
                "#!/bin/sh\nfor arg do\n if [ \"$arg\" = merge ]; then printf 'merge\\n' >> '{}' ; fi\ndone\nexec '{}' \"$@\"\n",
                trace.display(), real_git.trim()
            ),
        ).unwrap();
        fs::set_permissions(shim.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut g = Self {
            _tmp: tmp,
            repo,
            shim,
            trace,
            id: String::new(),
            worktree: PathBuf::new(),
        };
        g.git_ok(&["init", "-q", "-b", "main"]);
        g.git_ok(&["config", "user.email", "test@example.com"]);
        g.git_ok(&["config", "user.name", "Test"]);
        g.git_ok(&["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(g.repo.join(".cosmon/state")).unwrap();
        fs::create_dir_all(g.repo.join(".cosmon/formulas")).unwrap();
        fs::write(
            g.repo.join(".cosmon/config.toml"),
            "[project]\nproject_id = \"dirty-base-test\"\n",
        )
        .unwrap();
        fs::write(
            g.repo.join(".cosmon/state/fleet.json"),
            "{\"repos\":{},\"workers\":{}}\n",
        )
        .unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.cosmon/formulas/task-work.formula.toml"),
            g.repo.join(".cosmon/formulas/task-work.formula.toml"),
        )
        .unwrap();
        fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
        fs::write(g.repo.join("unrelated.txt"), "base\n").unwrap();
        g.git_ok(&["add", ".gitignore", "unrelated.txt"]);
        g.git_ok(&["commit", "-qm", "base"]);
        let born = g
            .cs()
            .args([
                "--json",
                "nucleate",
                "task-work",
                "--var",
                "topic=dirty base regression",
            ])
            .output()
            .unwrap();
        assert!(
            born.status.success(),
            "{}",
            String::from_utf8_lossy(&born.stderr)
        );
        let born: serde_json::Value = serde_json::from_slice(&born.stdout).unwrap();
        g.id = born["id"].as_str().unwrap().to_owned();
        let completed = g
            .cs()
            .args(["complete", &g.id, "--reason", "fixture ready"])
            .output()
            .unwrap();
        assert!(
            completed.status.success(),
            "{}",
            String::from_utf8_lossy(&completed.stderr)
        );
        g.worktree = g.repo.join(".worktrees").join(&g.id);
        g.git_ok(&[
            "worktree",
            "add",
            "-qb",
            &format!("feat/{}", g.id),
            g.worktree.to_str().unwrap(),
        ]);
        g
    }

    fn cs(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(&self.repo)
            .env(
                "PATH",
                format!("{}:{}", self.shim.display(), std::env::var("PATH").unwrap()),
            )
            .env("COSMON_STATE_DIR", self.repo.join(".cosmon/state"))
            .env("COSMON_CONFIG", self.repo.join(".cosmon/config.toml"));
        cmd
    }

    fn git(&self, args: &[&str]) -> Output {
        Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn git_ok(&self, args: &[&str]) {
        let out = self.git(args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn head(&self) -> Vec<u8> {
        self.git(&["rev-parse", "main"]).stdout
    }

    fn worker_changes(&self, path: &str, text: &str) {
        fs::write(self.worktree.join(path), text).unwrap();
        for args in [
            vec!["add", "--", path],
            vec!["commit", "-qm", "worker change"],
        ] {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.worktree)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    fn done(&self, json: bool) -> Output {
        let mut cmd = self.cs();
        if json {
            cmd.arg("--json");
        }
        cmd.args(["done", &self.id, "--no-auto-propel"])
            .output()
            .unwrap()
    }

    fn assert_preserved(&self, before: &[u8]) {
        assert_eq!(before, self.head(), "base must not advance");
        assert!(self.worktree.is_dir(), "worker worktree must survive");
        assert!(self
            .git(&["rev-parse", "--verify", &format!("feat/{}", self.id)])
            .status
            .success());
        assert!(!self.repo.join(".git/MERGE_HEAD").exists());
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(
                self.repo
                    .join(".cosmon/state/fleets/default/molecules")
                    .join(&self.id)
                    .join("state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(state["merged_at"].is_null());
        let harvestable = self
            .cs()
            .args(["--json", "peek", "--phase", "harvestable"])
            .output()
            .unwrap();
        assert!(harvestable.status.success());
        assert!(String::from_utf8_lossy(&harvestable.stdout).contains(&self.id));
    }
}

#[test]
fn done_dirty_base_refuses_overlap_before_merge_in_text_and_json() {
    for json in [false, true] {
        for staged in [false, true] {
            let g = Galaxy::new();
            g.worker_changes(".gitignore", ".cosmon/\n.worktrees/\nworker/\n");
            fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\npilot/\n").unwrap();
            if staged {
                g.git_ok(&["add", ".gitignore"]);
            }
            let before = g.head();
            let out = g.done(json);
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(!out.status.success(), "{text}");
            assert!(
                text.contains(".gitignore")
                    && text.contains("commit or stash")
                    && text.contains(&format!("cs done {}", g.id)),
                "actionable refusal required: {text}"
            );
            assert!(
                !g.trace.exists(),
                "preflight must refuse before any git merge: {text}"
            );
            if json {
                let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(v["merged"], false);
                assert_eq!(v["teardown"], false);
            }
            assert_eq!(
                fs::read_to_string(g.repo.join(".gitignore")).unwrap(),
                ".cosmon/\n.worktrees/\npilot/\n"
            );
            g.assert_preserved(&before);
            // The prescribed recovery is sufficient on the very same molecule.
            g.git_ok(&["stash", "push", "-qm", "pilot edits"]);
            let retry = g.done(json);
            assert!(
                retry.status.success(),
                "{}{}",
                String::from_utf8_lossy(&retry.stdout),
                String::from_utf8_lossy(&retry.stderr)
            );
            assert_ne!(before, g.head());
        }
    }
}

#[test]
fn done_dirty_base_allows_unrelated_local_edits() {
    let g = Galaxy::new();
    g.worker_changes(".gitignore", ".cosmon/\n.worktrees/\nworker/\n");
    fs::write(g.repo.join("unrelated.txt"), "pilot edits\n").unwrap();
    let before = g.head();
    let out = g.done(true);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(before, g.head());
    assert_eq!(
        fs::read_to_string(g.repo.join("unrelated.txt")).unwrap(),
        "pilot edits\n"
    );
    assert_eq!(
        fs::read_to_string(g.repo.join(".gitignore")).unwrap(),
        ".cosmon/\n.worktrees/\nworker/\n"
    );
    assert!(
        g.trace.exists(),
        "positive control must observe a real git merge"
    );
}

#[test]
fn done_dirty_base_allows_edits_to_a_base_only_change_on_diverged_history() {
    let g = Galaxy::new();
    g.worker_changes(".gitignore", ".cosmon/\n.worktrees/\nworker/\n");
    fs::write(g.repo.join("unrelated.txt"), "base advanced\n").unwrap();
    g.git_ok(&["commit", "-qam", "advance base independently"]);
    fs::write(g.repo.join("unrelated.txt"), "pilot edits\n").unwrap();
    let out = g.done(true);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(g.repo.join("unrelated.txt")).unwrap(),
        "pilot edits\n"
    );
    assert!(g.trace.exists());
}

#[test]
fn done_dirty_base_lists_every_overlap_including_unusual_names() {
    let g = Galaxy::new();
    let odd = "space and\nnewline.txt";
    fs::write(g.repo.join(odd), "base\n").unwrap();
    g.git_ok(&["add", "--", odd]);
    g.git_ok(&["commit", "-qm", "base unusual path"]);
    // Move the disposable worker to the advanced base before its changes.
    let out = Command::new("git")
        .arg("-C")
        .arg(&g.worktree)
        .args(["reset", "--hard", "main"])
        .output()
        .unwrap();
    assert!(out.status.success());
    g.worker_changes(".gitignore", ".cosmon/\n.worktrees/\nworker/\n");
    g.worker_changes(odd, "worker\n");
    fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\npilot/\n").unwrap();
    fs::write(g.repo.join(odd), "pilot\n").unwrap();
    let before = g.head();
    let out = g.done(true);
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let error = v["error"].as_str().unwrap();
    assert!(
        error.contains(".gitignore") && error.contains("space and\\nnewline.txt"),
        "{error}"
    );
    assert!(!g.trace.exists());
    g.assert_preserved(&before);
}

#[test]
fn done_dirty_base_preserves_the_conflict_path_for_unrelated_edits() {
    let g = Galaxy::new();
    g.worker_changes(".gitignore", ".cosmon/\n.worktrees/\nworker/\n");
    fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\nbase/\n").unwrap();
    g.git_ok(&["commit", "-qam", "conflicting base change"]);
    fs::write(g.repo.join("unrelated.txt"), "pilot edits\n").unwrap();
    let before = g.head();
    let out = g.done(true);
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["outcome"], "merge_conflict");
    assert!(
        g.trace.exists(),
        "unrelated edits must reach normal conflict handling"
    );
    assert_eq!(
        fs::read_to_string(g.repo.join("unrelated.txt")).unwrap(),
        "pilot edits\n"
    );
    g.assert_preserved(&before);
}

#[test]
fn done_dirty_base_refuses_a_staged_edit_with_worktree_restored_to_head() {
    let g = Galaxy::new();
    g.worker_changes(".gitignore", ".cosmon/\n.worktrees/\nworker/\n");
    fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\npilot/\n").unwrap();
    g.git_ok(&["add", ".gitignore"]);
    fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    let before = g.head();
    let out = g.done(true);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!out.status.success());
    assert!(v["error"].as_str().unwrap().contains("commit or stash"));
    assert!(!g.trace.exists());
    assert!(String::from_utf8_lossy(&g.git(&["show", ":.gitignore"]).stdout).contains("pilot/"));
    g.assert_preserved(&before);
}

#[test]
fn done_dirty_base_refuses_a_rename_over_a_local_edit() {
    let g = Galaxy::new();
    for args in [
        vec!["mv", ".gitignore", "renamed-ignore"],
        vec!["commit", "-qm", "worker rename"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&g.worktree)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\npilot/\n").unwrap();
    let before = g.head();
    let out = g.done(true);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!out.status.success());
    assert!(v["error"].as_str().unwrap().contains("commit or stash"));
    assert!(!g.trace.exists());
    g.assert_preserved(&before);
}

#[test]
fn done_dirty_base_refuses_dirty_modify_delete_conflict_before_merge() {
    let g = Galaxy::new();
    for args in [
        vec!["rm", "unrelated.txt"],
        vec!["commit", "-qm", "worker deletion"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&g.worktree)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    fs::write(g.repo.join("unrelated.txt"), "committed base edit\n").unwrap();
    g.git_ok(&["commit", "-qam", "base modification"]);
    fs::write(g.repo.join("unrelated.txt"), "pilot edits\n").unwrap();
    let before = g.head();
    let out = g.done(true);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!out.status.success());
    assert!(v["error"].as_str().unwrap().contains("commit or stash"));
    assert!(!g.trace.exists());
    g.assert_preserved(&before);
}

#[test]
fn done_dirty_base_allows_local_edit_when_both_tips_already_have_the_same_change() {
    let g = Galaxy::new();
    g.worker_changes(".gitignore", ".cosmon/\n.worktrees/\nsame/\n");
    fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\nsame/\n").unwrap();
    g.git_ok(&["commit", "-qam", "same base change"]);
    fs::write(g.repo.join(".gitignore"), ".cosmon/\n.worktrees/\npilot/\n").unwrap();
    let out = g.done(true);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(g.trace.exists());
    assert!(fs::read_to_string(g.repo.join(".gitignore"))
        .unwrap()
        .contains("pilot/"));
}
