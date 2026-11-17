// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #218: `cs done` must finish removing a worktree whose directory a
//! process is still writing into, within a bounded window, and must report an
//! honest warning (path + remedy) when the window runs out.
//!
//! `git worktree remove` deletes the directory tree and fails with
//! `failed to delete '<path>': Directory not empty` when a process creates
//! entries under it mid-walk; git has by then already dropped the
//! registration, so the directory is orphaned. The fixture drives the real
//! `cs` binary and a real `git`; a thin `git` shim only *schedules* when the
//! writer is told to stop, so the "stops inside the retry window" case does
//! not depend on how long the harvest took to reach the removal.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

struct Galaxy {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    id: String,
    worktree: PathBuf,
    writer: Option<Child>,
}

impl Drop for Galaxy {
    fn drop(&mut self) {
        self.stop_writer();
    }
}

impl Galaxy {
    /// `stop_after`: seconds after the first `git worktree remove` at which
    /// the shim tells the writer to stop; `None` leaves the writer running.
    fn new(stop_after: Option<u32>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("galaxy");
        let shim = root.join("bin");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&shim).unwrap();
        let real_git = Command::new("which").arg("git").output().unwrap();
        let real_git = String::from_utf8(real_git.stdout).unwrap();
        let schedule = stop_after.map_or(String::new(), |s| {
            format!(
                "   if [ ! -e '{r}/scheduled' ]; then : > '{r}/scheduled'; \
                 (sleep {s}; : > '{r}/stop') >/dev/null 2>&1 </dev/null & fi\n",
                r = root.display()
            )
        });
        fs::write(
            shim.join("git"),
            format!(
                "#!/bin/sh\ncase \"$*\" in\n *'worktree remove'*)\n{schedule};;\nesac\nexec '{}' \"$@\"\n",
                real_git.trim()
            ),
        )
        .unwrap();
        fs::set_permissions(shim.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut g = Self {
            _tmp: tmp,
            root,
            repo,
            id: String::new(),
            worktree: PathBuf::new(),
            writer: None,
        };
        g.git_ok(&["init", "-q", "-b", "main"]);
        g.git_ok(&["config", "user.email", "test@example.com"]);
        g.git_ok(&["config", "user.name", "Test"]);
        g.git_ok(&["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(g.repo.join(".cosmon/state")).unwrap();
        fs::create_dir_all(g.repo.join(".cosmon/formulas")).unwrap();
        fs::write(
            g.repo.join(".cosmon/config.toml"),
            "[project]\nproject_id = \"worktree-race-test\"\n",
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
        fs::write(
            g.repo.join(".gitignore"),
            ".cosmon/\n.worktrees/\ntarget\nescape\n",
        )
        .unwrap();
        fs::write(g.repo.join("unrelated.txt"), "base\n").unwrap();
        g.git_ok(&["add", ".gitignore", "unrelated.txt"]);
        g.git_ok(&["commit", "-qm", "base"]);
        let born = g
            .cs()
            .args(["--json", "nucleate", "task-work", "--var", "topic=race"])
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
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("COSMON_STATE_DIR", self.repo.join(".cosmon/state"))
            .env("COSMON_CONFIG", self.repo.join(".cosmon/config.toml"));
        cmd
    }

    fn git_ok(&self, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Start a process that keeps creating entries under the worktree's
    /// `target/` until `<root>/stop` exists.
    fn start_writer(&mut self) {
        let target = self.worktree.join("target");
        fs::create_dir_all(&target).unwrap();
        // Pre-populate so git's recursive delete takes long enough for the
        // writer to land entries mid-walk, then keep adding entries. The
        // writer never re-creates `target/` itself: once the directory is
        // gone it exits, so a successful removal is not undone by the fixture.
        for n in 0..3000 {
            let dir = target.join(format!("pre{n}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("f"), "").unwrap();
        }
        let script = format!(
            "i=0; while [ ! -e '{r}/stop' ] && [ -d '{t}' ]; do mkdir '{t}/d'$i 2>/dev/null; \
             : > '{t}/d'$i/f 2>/dev/null; i=$((i+1)); done",
            r = self.root.display(),
            t = target.display()
        );
        self.writer = Some(
            Command::new("sh")
                .arg("-c")
                .arg(script)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    fn stop_writer(&mut self) {
        let _ = fs::write(self.root.join("stop"), "");
        if let Some(mut w) = self.writer.take() {
            let _ = w.kill();
            let _ = w.wait();
        }
    }

    fn done(&self) -> Output {
        self.cs()
            .args(["done", &self.id, "--no-auto-propel"])
            .output()
            .unwrap()
    }
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// (i) A writer that stops inside the retry window: the directory is gone and
/// the harvest reports no removal warning.
#[test]
fn writer_that_stops_within_the_window_leaves_no_directory_and_no_warning() {
    let mut g = Galaxy::new(Some(2));
    g.start_writer();
    let out = g.done();
    let text = combined(&out);
    assert!(out.status.success(), "cs done must succeed.\n{text}");
    assert!(
        !g.worktree.exists(),
        "the worktree directory must be gone.\n{text}"
    );
    assert!(
        !text.contains("worktree remove failed"),
        "no removal warning expected.\n{text}"
    );
}

/// (ii) A writer that never stops: `cs done` still exits 0, the warning names
/// the path and the remedy, and `cs purge --worktrees` lists the leftover.
#[test]
fn writer_that_never_stops_warns_with_path_and_remedy() {
    let mut g = Galaxy::new(None);
    g.start_writer();
    let out = g.done();
    let text = combined(&out);
    assert!(out.status.success(), "cs done must succeed.\n{text}");
    assert!(g.worktree.exists(), "the writer keeps the directory alive");
    assert!(
        text.contains("worktree remove failed")
            && text.contains(&g.worktree.display().to_string())
            && text.contains("still writing")
            && text.contains("cs purge --worktrees"),
        "the warning must carry the path, the cause and the remedy.\n{text}"
    );
    g.stop_writer();
    let purge = g
        .cs()
        .args(["purge", "--worktrees"])
        .output()
        .expect("cs purge");
    let listing = combined(&purge);
    assert!(
        listing.contains(&g.id),
        "the leftover must be listed by the dry run.\n{listing}"
    );
    eprintln!("purge --worktrees dry run:\n{listing}");
}

/// (iii) A symlink inside the worktree pointing outside it is removed as a
/// link; the contents it pointed at are untouched.
#[test]
fn symlinked_target_is_unlinked_and_its_destination_survives() {
    let mut g = Galaxy::new(Some(2));
    let outside = g.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "must survive").unwrap();
    std::os::unix::fs::symlink(&outside, g.worktree.join("escape")).unwrap();
    g.start_writer();
    let out = g.done();
    let text = combined(&out);
    assert!(out.status.success(), "cs done must succeed.\n{text}");
    assert!(!g.worktree.exists(), "worktree must be gone.\n{text}");
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "must survive",
        "the link target must be untouched.\n{text}"
    );
}
