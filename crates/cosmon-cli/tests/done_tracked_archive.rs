// SPDX-License-Identifier: AGPL-3.0-only

//! A tracked archive must not leave the next sibling harvest with a dirty index.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

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
        .expect("spawn git")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn cs(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cs"))
        .args(args)
        .current_dir(repo)
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_BASE_BRANCH")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .output()
        .expect("spawn cs")
}

fn terminal_molecule(repo: &Path) -> String {
    let out = cs(
        repo,
        &[
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=tracked archive",
        ],
    );
    assert!(
        out.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("nucleate JSON");
    let id = value["id"].as_str().expect("molecule id").to_owned();
    let out = cs(repo, &["collapse", &id, "--reason", "fixture"]);
    assert!(
        out.status.success(),
        "collapse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    id
}

fn tracked_archive_fixture() -> (tempfile::TempDir, String, String) {
    let tmp = tempfile::tempdir().expect("temp repo");
    let repo = tmp.path();
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.name", "cosmon"]);
    git_ok(repo, &["config", "user.email", "cosmon@example.invalid"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);

    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state/archive/events")).expect("archive dir");
    fs::create_dir_all(cosmon.join("formulas")).expect("formula dir");
    fs::write(
        cosmon.join("config.toml"),
        "[project]\nproject_id = \"tracked-archive\"\n\n[archive]\nenabled = true\n",
    )
    .expect("config");
    fs::write(cosmon.join("state/fleet.json"), "{}\n").expect("fleet");
    fs::write(
        cosmon.join(".gitignore"),
        "state/*\n!state/archive/\n!state/frontier.json\n",
    )
    .expect("ignore");
    fs::write(cosmon.join("state/frontier.json"), "seed\n").expect("frontier seed");
    let archive_events = format!("events-{}.jsonl", chrono::Utc::now().format("%Y-%m"));
    fs::write(
        cosmon.join("state/archive/events").join(archive_events),
        "seed\n",
    )
    .expect("archive seed");
    fs::write(cosmon.join("state/events.jsonl"), "").expect("tracked events");
    fs::write(repo.join(".gitignore"), ".worktrees/\n").expect("root ignore");
    fs::write(repo.join("base.txt"), "base\n").expect("base");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml"),
        cosmon.join("formulas/task-work.formula.toml"),
    )
    .expect("formula");
    git_ok(repo, &["add", ".gitignore", ".cosmon", "base.txt"]);
    git_ok(repo, &["add", "-f", ".cosmon/state/events.jsonl"]);
    git_ok(repo, &["commit", "-qm", "base"]);
    assert!(
        git(repo, &["check-ignore", ".cosmon/state/fleet.json"])
            .status
            .success(),
        "state/* must ignore runtime state"
    );
    assert!(
        !git(
            repo,
            &[
                "check-ignore",
                "--no-index",
                ".cosmon/state/archive/2026/09/example/edges.json"
            ]
        )
        .status
        .success(),
        "the archive subtree must remain trackable"
    );

    let a = terminal_molecule(repo);
    let b = terminal_molecule(repo);
    for (id, file) in [(&a, "a.txt"), (&b, "b.txt")] {
        git_ok(repo, &["checkout", "-q", "-b", &format!("feat/{id}")]);
        fs::write(repo.join(file), id).expect("worker file");
        git_ok(repo, &["add", file]);
        git_ok(repo, &["commit", "-qm", "worker output"]);
        git_ok(repo, &["checkout", "-q", "main"]);
    }

    (tmp, a, b)
}

#[test]
fn sibling_harvests_leave_tracked_archive_index_clean() {
    let (tmp, a, b) = tracked_archive_fixture();
    let repo = tmp.path();
    let mut dirty_after_done = Vec::new();

    for (id, file) in [(&a, "a.txt"), (&b, "b.txt")] {
        let done = cs(repo, &["--json", "done", id, "--no-auto-propel"]);
        assert!(
            done.status.success(),
            "done {id}: stdout={} stderr={}",
            String::from_utf8_lossy(&done.stdout),
            String::from_utf8_lossy(&done.stderr)
        );
        let stdout = String::from_utf8_lossy(&done.stdout);
        let result: serde_json::Value = serde_json::from_str(stdout.trim()).expect("done JSON");
        assert!(
            result["warnings"]
                .as_array()
                .is_some_and(|warnings| warnings.iter().all(|warning| {
                    !warning
                        .as_str()
                        .unwrap_or("")
                        .contains("artifact commit failed")
                })),
            "ignored molecule directory produced an artifact warning: {result}"
        );
        assert!(
            git(repo, &["cat-file", "-e", &format!("main:{file}")])
                .status
                .success(),
            "{file} did not merge"
        );
        let archived = git(
            repo,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                "main",
                "--",
                ".cosmon/state/archive",
            ],
        );
        assert!(archived.status.success());
        assert!(
            String::from_utf8_lossy(&archived.stdout).contains(id.as_str()),
            "archive entry for {id} was not committed"
        );
        let staged = git(repo, &["diff", "--cached", "--name-only"]);
        assert!(staged.status.success());
        assert!(
            staged.stdout.is_empty(),
            "done {id} left staged paths: {}",
            String::from_utf8_lossy(&staged.stdout)
        );
        let dirty = git(
            repo,
            &["status", "--porcelain", "--", ".cosmon/state/archive"],
        );
        assert!(dirty.status.success());
        dirty_after_done.push(String::from_utf8_lossy(&dirty.stdout).into_owned());
    }
    assert!(
        dirty_after_done.iter().all(String::is_empty),
        "harvest left tracked or stageable archive changes: {dirty_after_done:?}"
    );
}

#[test]
fn sibling_harvest_cannot_enter_trunk_during_post_harvest_commit() {
    let (tmp, a, b) = tracked_archive_fixture();
    let repo = tmp.path();
    let entered = repo.join("post-commit-entered");
    let release = repo.join("release-post-commit");
    let hook = repo.join(".git/hooks/prepare-commit-msg");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\ncase \"$(cat \"$1\")\" in\n  *'record harvest state'*)\n    if mkdir \"{}\" 2>/dev/null; then\n      touch \"{}\"\n      while test ! -f \"{}\"; do sleep 0.05; done\n    fi\n    ;;\nesac\n",
            repo.join("post-commit-once").display(),
            entered.display(),
            release.display(),
        ),
    )
    .expect("hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("executable hook");
    }

    let first = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args(["--json", "done", &a, "--no-auto-propel"])
        .current_dir(repo)
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_BASE_BRANCH")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("first harvest");
    let started = Instant::now();
    while !entered.exists() && started.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !entered.exists() {
        let _ = fs::write(&release, "go");
        let output = first.wait_with_output().expect("first result");
        panic!("first harvest must reach the state commit: {output:?}");
    }

    let sibling = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args(["--json", "done", &b, "--no-auto-propel"])
        .current_dir(repo)
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env("COSMON_TRUNK_LOCK_NONBLOCKING", "1")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_BASE_BRANCH")
        .output()
        .expect("sibling harvest");
    fs::write(&release, "go").expect("release first harvest");
    let first_output = first.wait_with_output().expect("first result");
    assert!(
        first_output.status.success(),
        "first harvest: {first_output:?}"
    );
    assert!(
        !sibling.status.success() && String::from_utf8_lossy(&sibling.stderr).contains("trunk"),
        "sibling must be refused by the held trunk lock: {sibling:?}"
    );
    let retry = cs(repo, &["--json", "done", &b, "--no-auto-propel"]);
    assert!(retry.status.success(), "sibling retry: {retry:?}");
}
