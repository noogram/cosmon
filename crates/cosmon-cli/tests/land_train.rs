// SPDX-License-Identifier: AGPL-3.0-only

//! `cs land-train` lands an operator-built, operator-gated train on the base
//! branch, then closes each member with `cs done --expect-head` (issue #213,
//! ADR-190 D3, increment D1).
//!
//! Every test runs the real `cs` binary against a throwaway repository with a
//! repository-local identity. The train is built the way ADR-190 requires an
//! operator tool to build it: a side branch holding one `git merge --no-ff`
//! per member, each with the message `cs done --print-merge-message` prints
//! (`-m "$(cs done <id> --print-merge-message)"`).
//! Each refusal fixture is that happy-path train plus exactly one defect, and
//! each is refuted if the base moves or a member is closed (ADR-190 §4 D1).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A `cs` command pinned to the fixture's state, config and trust store. The
/// trust bypass is removed so the shell-surface predicate really runs, and the
/// trunk lock fails fast instead of blocking if anything holds it across a
/// closure.
fn cs(repo: &Path, trust_store: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_BASE_BRANCH")
        .env_remove("COSMON_ASSUME_TRUSTED")
        .env_remove("COSMON_SKIP_PRE_DONE_HOOK")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env("COSMON_TRUNK_LOCK_NONBLOCKING", "1")
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env("COSMON_TRUST_DIR", trust_store)
        .env("GIT_PAGER", "cat")
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

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn rev(repo: &Path, name: &str) -> String {
    git_ok(repo, &["rev-parse", name])
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A fixture galaxy. `tracked_cosmon` tracks `.cosmon/config.toml` and the
/// formulas, so a member can change the shell surface through its merge;
/// otherwise `.cosmon/` is ignored and no train can touch the surface.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    trust: PathBuf,
}

impl Fixture {
    fn new(config: &str, tracked_cosmon: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let trust = tmp.path().join("trust");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&trust).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        git_ok(&repo, &["config", "user.email", "test@example.com"]);
        git_ok(&repo, &["config", "user.name", "Test"]);
        git_ok(&repo, &["config", "commit.gpgsign", "false"]);
        let cosmon = repo.join(".cosmon");
        fs::create_dir_all(cosmon.join("state")).unwrap();
        fs::create_dir_all(cosmon.join("formulas")).unwrap();
        fs::write(
            cosmon.join("config.toml"),
            format!("[project]\nproject_id = \"test-land-train\"\n{config}"),
        )
        .unwrap();
        fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
        let formula_src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cosmon/formulas/task-work.formula.toml");
        fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();
        let ignore = if tracked_cosmon {
            ".cosmon/state/\n.worktrees/\n"
        } else {
            ".cosmon/\n.worktrees/\n"
        };
        fs::write(repo.join(".gitignore"), ignore).unwrap();
        fs::write(repo.join("base.txt"), "base\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-q", "-m", "base"]);
        Self {
            _tmp: tmp,
            repo,
            trust,
        }
    }

    /// A galaxy that tracks part of `.cosmon/state` (archive, frontier,
    /// events), as cosmon itself does: there `cs done` commits harvest state
    /// to the base after a closure, so the base moves past the train tip.
    fn new_tracked_state() -> Self {
        let fx = Self::new("[archive]\nenabled = true\n", true);
        let cosmon = fx.repo.join(".cosmon");
        fs::create_dir_all(cosmon.join("state/archive/events")).unwrap();
        fs::write(
            cosmon.join(".gitignore"),
            "state/*\n!state/archive/\n!state/frontier.json\n",
        )
        .unwrap();
        fs::write(cosmon.join("state/frontier.json"), "seed\n").unwrap();
        fs::write(cosmon.join("state/archive/events/seed.jsonl"), "seed\n").unwrap();
        fs::write(cosmon.join("state/events.jsonl"), "").unwrap();
        fs::write(fx.repo.join(".gitignore"), ".worktrees/\n").unwrap();
        git_ok(&fx.repo, &["add", "-A"]);
        git_ok(&fx.repo, &["add", "-f", ".cosmon/state/events.jsonl"]);
        git_ok(&fx.repo, &["commit", "-q", "-m", "track harvest state"]);
        fx
    }

    fn cs(&self) -> Command {
        cs(&self.repo, &self.trust)
    }

    /// Nucleate a `task-work` molecule and complete it.
    fn completed_molecule(&self, topic: &str) -> String {
        let mol_id = self.nucleate(topic, &[]);
        self.complete(&mol_id);
        mol_id
    }

    fn nucleate(&self, topic: &str, extra: &[&str]) -> String {
        let out = self
            .cs()
            .args(["--json", "nucleate", "task-work", "--var"])
            .arg(format!("topic={topic}"))
            .args(extra)
            .output()
            .expect("cs nucleate");
        assert!(out.status.success(), "nucleate failed: {}", combined(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["id"].as_str().expect("nucleate id").to_owned()
    }

    fn complete(&self, mol_id: &str) {
        let done = self
            .cs()
            .args(["complete", mol_id, "--reason", "test", "--ignore-mindguard"])
            .output()
            .expect("cs complete");
        assert!(
            done.status.success(),
            "complete failed: {}",
            combined(&done)
        );
    }

    /// Commit `file` with `contents` on `feat/<mol>` (created from `from` if
    /// absent) and return to `main`; returns the new branch tip.
    fn commit_on_branch(&self, mol_id: &str, from: &str, file: &str, contents: &str) -> String {
        let branch = format!("feat/{mol_id}");
        let exists = git(&self.repo, &["rev-parse", "--verify", "-q", &branch])
            .status
            .success();
        if exists {
            git_ok(&self.repo, &["checkout", "-q", &branch]);
        } else {
            git_ok(&self.repo, &["checkout", "-q", "-b", &branch, from]);
        }
        let path = self.repo.join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        git_ok(&self.repo, &["add", file]);
        git_ok(&self.repo, &["commit", "-qm", &format!("feat: {file}")]);
        let tip = rev(&self.repo, "HEAD");
        git_ok(&self.repo, &["checkout", "-q", "main"]);
        tip
    }

    /// What `cs done <mol> --print-merge-message` prints.
    fn merge_message(&self, mol_id: &str) -> String {
        let out = self
            .cs()
            .args(["done", mol_id, "--print-merge-message"])
            .output()
            .expect("cs done --print-merge-message");
        assert!(
            out.status.success(),
            "--print-merge-message failed: {}",
            combined(&out)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// On branch `train` (created from `main` if absent), merge `feat/<mol>`
    /// `--no-ff` with exactly the message `cs done` would write, passed as
    /// the shell form `-m "$(cs done <mol> --print-merge-message)"` would pass
    /// it: trailing newlines stripped, one `-m`. That form, unlike `-F` of the
    /// printed file, gives a sign-off hook the same input as `cs done`'s own
    /// merge (see `a_train_built_under_a_sign_off_hook_lands`).
    fn merge_member_into_train(&self, mol_id: &str) {
        self.checkout_train();
        let message = self.merge_message(mol_id);
        git_ok(
            &self.repo,
            &[
                "merge",
                "--no-ff",
                "-q",
                "-m",
                message.trim_end_matches('\n'),
                &format!("feat/{mol_id}"),
            ],
        );
        git_ok(&self.repo, &["checkout", "-q", "main"]);
    }

    fn checkout_train(&self) {
        let exists = git(&self.repo, &["rev-parse", "--verify", "-q", "train"])
            .status
            .success();
        if exists {
            git_ok(&self.repo, &["checkout", "-q", "train"]);
        } else {
            git_ok(&self.repo, &["checkout", "-q", "-b", "train", "main"]);
        }
    }

    fn land(&self, base: &str, tip: &str, members: &[(&str, &str)]) -> Output {
        self.land_command(base, tip, members)
            .output()
            .expect("cs land-train")
    }

    fn land_command(&self, base: &str, tip: &str, members: &[(&str, &str)]) -> Command {
        let mut cmd = self.cs();
        cmd.args(["land-train", "--train-base", base, "--train-tip", tip]);
        for (mol, head) in members {
            cmd.arg("--member").arg(format!("{mol}={head}"));
        }
        cmd
    }

    fn state(&self, mol_id: &str) -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(
                self.repo
                    .join(".cosmon/state/fleets/default/molecules")
                    .join(mol_id)
                    .join("state.json"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn is_closed(&self, mol_id: &str) -> bool {
        !self.state(mol_id)["merged_at"].is_null()
    }

    fn is_ancestor(&self, commit: &str, of: &str) -> bool {
        git(&self.repo, &["merge-base", "--is-ancestor", commit, of])
            .status
            .success()
    }
}

/// Two completed members A and B, each with one commit on its branch, and a
/// train holding a `--no-ff` merge of each in that order.
struct Train {
    fx: Fixture,
    a: String,
    b: String,
    ha: String,
    hb: String,
    base: String,
    tip: String,
}

impl Train {
    fn members(&self) -> Vec<(&str, &str)> {
        vec![(&self.a, &self.ha), (&self.b, &self.hb)]
    }

    fn land(&self) -> Output {
        self.fx.land(&self.base, &self.tip, &self.members())
    }

    fn check(&self) -> Output {
        self.fx
            .land_command(&self.base, &self.tip, &self.members())
            .arg("--check")
            .output()
            .unwrap()
    }

    fn land_through_git(&self, shim_script: &str) -> Command {
        let shim_dir = self.fx._tmp.path().join("bin");
        fs::create_dir(&shim_dir).unwrap();
        let shim = shim_dir.join("git");
        fs::write(&shim, shim_script).unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var_os("PATH").unwrap();
        let shim_path =
            std::env::join_paths(std::iter::once(shim_dir).chain(std::env::split_paths(&path)))
                .unwrap();
        let mut cmd = self.fx.land_command(&self.base, &self.tip, &self.members());
        cmd.env("PATH", shim_path).env("LAND_TRAIN_REAL_PATH", path);
        cmd
    }

    /// The refusal invariants every defect test shares: the command failed,
    /// its output names `needle`, the base did not move and no member closed.
    fn assert_refused_naming(&self, out: &Output, needle: &str, main_before: &str) {
        let text = combined(out);
        assert!(!out.status.success(), "the train must be refused: {text}");
        assert!(
            text.contains(needle),
            "the refusal must name `{needle}`: {text}"
        );
        assert_eq!(
            rev(&self.fx.repo, "main"),
            main_before,
            "the base must not move on a refusal"
        );
        assert!(!self.fx.is_closed(&self.a), "member A must not be closed");
        assert!(!self.fx.is_closed(&self.b), "member B must not be closed");
    }
}

/// Members A and B committed off `main`, not yet in any train.
fn two_members(fx: Fixture) -> (Fixture, String, String, String, String) {
    let a = fx.completed_molecule("member a");
    let b = fx.completed_molecule("member b");
    let ha = fx.commit_on_branch(&a, "main", "a.txt", "a\n");
    let hb = fx.commit_on_branch(&b, "main", "b.txt", "b\n");
    (fx, a, b, ha, hb)
}

/// The happy-path train over a fixture.
fn happy_train(fx: Fixture) -> Train {
    let (fx, a, b, ha, hb) = two_members(fx);
    let base = rev(&fx.repo, "main");
    fx.merge_member_into_train(&a);
    fx.merge_member_into_train(&b);
    let tip = rev(&fx.repo, "train");
    Train {
        fx,
        a,
        b,
        ha,
        hb,
        base,
        tip,
    }
}

/// Three train members. Only the middle member descends from the removed
/// member's pinned head. The removed member is absent from the train list.
struct CarrierTrain {
    fx: Fixture,
    base: String,
    tip: String,
    members: [(String, String); 3],
    excluded: (String, String),
}

impl CarrierTrain {
    fn new() -> Self {
        Self::from_fixture(Fixture::new("", false))
    }

    fn from_fixture(fx: Fixture) -> Self {
        let base = rev(&fx.repo, "main");
        let removed = fx.completed_molecule("removed member");
        let removed_head = fx.commit_on_branch(&removed, &base, "removed.txt", "removed\n");
        let members = ["first", "middle", "last"].map(|name| {
            let id = fx.completed_molecule(name);
            let mut head = fx.commit_on_branch(&id, &base, &format!("{name}.txt"), "member\n");
            if name == "middle" {
                git_ok(&fx.repo, &["checkout", "-q", &format!("feat/{id}")]);
                git_ok(
                    &fx.repo,
                    &[
                        "merge",
                        "--no-ff",
                        "-qm",
                        "merge removed member",
                        &removed_head,
                    ],
                );
                head = rev(&fx.repo, "HEAD");
                git_ok(&fx.repo, &["checkout", "-q", "main"]);
            }
            fx.merge_member_into_train(&id);
            (id, head)
        });
        let tip = rev(&fx.repo, "train");
        Self {
            fx,
            base,
            tip,
            members,
            excluded: (removed, removed_head),
        }
    }

    fn land(&self, exclusions: &[(&str, &str)]) -> Output {
        let members = self
            .members
            .each_ref()
            .map(|(id, head)| (id.as_str(), head.as_str()));
        let mut cmd = self.fx.land_command(&self.base, &self.tip, &members);
        for (id, head) in exclusions {
            cmd.arg("--exclude").arg(format!("{id}={head}"));
        }
        cmd.output().expect("cs land-train")
    }

    fn assert_refusal(&self, out: &Output, needles: &[&str]) {
        let output = combined(out);
        assert!(!out.status.success(), "the train must be refused: {output}");
        assert_eq!(rev(&self.fx.repo, "main"), self.base);
        for (id, _) in self.members.iter().chain([&self.excluded]) {
            assert!(!self.fx.is_closed(id), "{id} must stay open");
        }
        for needle in needles {
            assert!(output.contains(needle), "expected `{needle}`: {output}");
        }
    }

    fn assert_landed(&self, out: &Output) {
        assert!(out.status.success(), "landing failed: {}", combined(out));
        assert_eq!(rev(&self.fx.repo, "main"), self.tip);
        for (id, _) in &self.members {
            assert!(self.fx.is_closed(id), "{id} must be closed");
        }
        assert!(!self.fx.is_closed(&self.excluded.0));
    }
}

#[test]
fn exclusion_refuses_the_middle_carrier_without_landing_or_closing() {
    let train = CarrierTrain::new();
    let (id, head) = &train.excluded;
    train
        .fx
        .commit_on_branch(id, &train.base, "later.txt", "later\n");
    let out = train.land(&[(id, head)]);
    train.assert_refusal(
        &out,
        &["carries excluded member", &train.members[1].0, id, head],
    );
}

#[test]
fn exclusion_checks_later_exclusions_after_an_unrelated_one() {
    let train = CarrierTrain::new();
    let (id, head) = &train.excluded;
    let other = train.fx.completed_molecule("other removed member");
    let unrelated = train
        .fx
        .commit_on_branch(&other, &train.base, "other.txt", "other\n");
    let out = train.land(&[(&other, &unrelated), (id, head)]);
    train.assert_refusal(
        &out,
        &["carries excluded member", &train.members[1].0, id, head],
    );
}

#[test]
fn without_exclusions_the_same_train_lands_and_closes_its_members() {
    let train = CarrierTrain::new();
    train.assert_landed(&train.land(&[]));
}

#[test]
fn exclusion_refuses_a_carrier_of_an_earlier_commit_than_the_pinned_head() {
    let train = CarrierTrain::new();
    let (id, first_commit) = &train.excluded;
    let head = train
        .fx
        .commit_on_branch(id, &train.base, "later.txt", "later\n");
    assert!(train.fx.is_ancestor(first_commit, &train.members[1].1));
    assert!(!train.fx.is_ancestor(&head, &train.members[1].1));
    train.assert_refusal(
        &train.land(&[(id, &head)]),
        &[
            "carries excluded member",
            &train.members[1].0,
            id,
            first_commit,
        ],
    );
}

#[test]
fn exclusion_allows_a_branch_at_the_train_base_with_no_own_commits() {
    let train = CarrierTrain::new();
    let id = train.fx.completed_molecule("empty removed branch");
    git_ok(
        &train.fx.repo,
        &["branch", &format!("feat/{id}"), &train.base],
    );
    train.assert_landed(&train.land(&[(&id, &train.base)]));
    assert!(!train.fx.is_closed(&id));
}

#[test]
fn exclusion_allows_a_head_already_merged_into_the_train_base() {
    let fx = Fixture::new("", false);
    let id = fx.completed_molecule("already on the base");
    let head = fx.commit_on_branch(&id, "main", "already.txt", "already on the base\n");
    git_ok(
        &fx.repo,
        &["merge", "--no-ff", "-qm", "merge into base", &head],
    );
    let train = CarrierTrain::from_fixture(fx);
    assert_eq!(head, rev(&train.fx.repo, "main^2"));
    train.assert_landed(&train.land(&[(&id, &head)]));
    assert!(!train.fx.is_closed(&id));
}

#[test]
fn exclusion_refuses_an_id_also_listed_as_a_member() {
    let train = CarrierTrain::new();
    let (id, _) = &train.members[0];
    train.assert_refusal(
        &train.land(&[(id, &train.base)]),
        &[id, "also listed as a train member"],
    );
}

#[test]
fn exclusion_refuses_an_unknown_pinned_head() {
    let train = CarrierTrain::new();
    let (id, _) = &train.excluded;
    let unknown = "f".repeat(train.base.len());
    train.assert_refusal(
        &train.land(&[(id, &unknown)]),
        &["excluded head", id, &unknown],
    );
}

#[test]
fn exclusion_refuses_a_missing_id_or_head() {
    let train = happy_train(Fixture::new("", false));
    for malformed in [
        train.a.clone(),
        format!("={}", train.ha),
        format!("{}=", train.a),
    ] {
        let out = train
            .fx
            .land_command(&train.base, &train.tip, &train.members())
            .args(["--exclude", &malformed])
            .output()
            .unwrap();
        train.assert_refused_naming(
            &out,
            "expected `<molecule id>=<pinned branch head>`",
            &train.base,
        );
        assert!(combined(&out).contains(&format!("--exclude `{malformed}`")));
    }
}

#[test]
fn exclusion_refuses_when_git_cannot_check_ancestry() {
    let train = happy_train(Fixture::new("", false));
    let id = train.fx.completed_molecule("removed member");
    let head = train
        .fx
        .commit_on_branch(&id, &train.base, "removed.txt", "removed\n");
    let out = train.land_through_git(r#"#!/bin/sh
set -eu
export PATH="$LAND_TRAIN_REAL_PATH"
if [ "$#" -ge 6 ] && [ "$3" = merge-base ] && [ "$4" = --is-ancestor ] && [ "$5" = "$LAND_TRAIN_EXCLUDED_HEAD" ]; then
    echo 'test ancestry failure' >&2
    exit 128
fi
exec git "$@"
"#)
        .env("LAND_TRAIN_EXCLUDED_HEAD", &head)
        .arg("--exclude")
        .arg(format!("{id}={head}"))
        .output()
        .unwrap();
    train.assert_refused_naming(&out, "test ancestry failure", &train.base);
    let text = combined(&out);
    for needle in [&id, &head, &train.a] {
        assert!(text.contains(needle), "expected `{needle}`: {text}");
    }
}

#[test]
fn exclusion_refuses_when_git_cannot_read_own_commits() {
    let train = happy_train(Fixture::new("", false));
    let id = train.fx.completed_molecule("removed member");
    let head = train
        .fx
        .commit_on_branch(&id, &train.base, "removed.txt", "removed\n");
    let out = train
        .land_through_git(r#"#!/bin/sh
set -eu
export PATH="$LAND_TRAIN_REAL_PATH"
if [ "$#" -eq 5 ] && [ "$3" = rev-list ] && [ "$4" = --first-parent ] && [ "$5" = "$LAND_TRAIN_EXCLUDED_RANGE" ]; then
    echo 'test own-commit range failure' >&2
    exit 128
fi
exec git "$@"
"#)
        .env("LAND_TRAIN_EXCLUDED_RANGE", format!("{}..{head}", train.base))
        .arg("--exclude")
        .arg(format!("{id}={head}"))
        .output()
        .unwrap();
    train.assert_refused_naming(&out, "test own-commit range failure", &train.base);
    let text = combined(&out);
    for needle in [&id, &head] {
        assert!(text.contains(needle), "expected `{needle}`: {text}");
    }
}

/// `(tree, parent count, second parent, raw message)` of every first-parent
/// commit in `from..to`, oldest first: the shape two histories must share.
fn first_parent_shape(repo: &Path, from: &str, to: &str) -> Vec<(String, usize, String, String)> {
    let range = format!("{from}..{to}");
    git_ok(
        repo,
        &["rev-list", "--first-parent", "--reverse", range.as_str()],
    )
    .lines()
    .map(|c| {
        let parents: Vec<String> = git_ok(repo, &["rev-list", "--parents", "-n1", c])
            .split_whitespace()
            .skip(1)
            .map(str::to_owned)
            .collect();
        let raw = git_ok(repo, &["cat-file", "commit", c]);
        let message = raw.split_once("\n\n").map_or("", |(_, m)| m).to_owned();
        (
            rev(repo, &format!("{c}^{{tree}}")),
            parents.len(),
            parents.get(1).cloned().unwrap_or_default(),
            message,
        )
    })
    .collect()
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

/// Happy path: two members, the base advances to the train tip by a
/// fast-forward, both members are closed by `cs done` (which stamps
/// `merged_at`), and the base's history has exactly the shape two serial
/// `cs done` merges would have given it.
#[test]
fn a_gated_train_lands_and_each_member_is_closed_once() {
    lands_like_two_serial_dones(Fixture::new("", false));
}

/// Under a `prepare-commit-msg` hook that appends `Signed-off-by:` after a
/// blank line — this repository's own DCO hook — both the operator's train
/// merges and `cs done`'s merges carry the trailer, so the train must still
/// land and still match two serial `cs done` merges.
#[test]
fn a_train_built_under_a_sign_off_hook_lands() {
    let fx = Fixture::new("", false);
    let hook = fx.repo.join(".git/hooks/prepare-commit-msg");
    fs::write(
        &hook,
        "#!/bin/sh\ngrep -qi '^Signed-off-by:' \"$1\" && exit 0\n\
         printf '\\nSigned-off-by: %s <%s>\\n' \"$(git config user.name)\" \
         \"$(git config user.email)\" >> \"$1\"\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&hook, perms).unwrap();
    let tip_message = lands_like_two_serial_dones(fx);
    assert!(
        tip_message.contains("\nSigned-off-by: Test <test@example.com>"),
        "the hook ran on the train merge: {tip_message}"
    );
}

/// Build the happy-path train over `fx`, land it, and check it against a twin
/// galaxy closed by two serial `cs done`. Returns the tip merge's message.
fn lands_like_two_serial_dones(fx: Fixture) -> String {
    let t = happy_train(fx);

    // A twin galaxy, byte-identical before the landing, closed the serial way.
    let twin_parent = tempfile::tempdir().unwrap();
    let twin = twin_parent.path().join("repo");
    copy_dir(&t.fx.repo, &twin);
    let twin_trust = twin_parent.path().join("trust");
    fs::create_dir_all(&twin_trust).unwrap();
    for mol in [&t.a, &t.b] {
        let out = cs(&twin, &twin_trust)
            .args(["done", mol, "--no-auto-propel"])
            .output()
            .unwrap();
        assert!(out.status.success(), "serial cs done: {}", combined(&out));
    }

    let landed = t.land();
    assert!(
        landed.status.success(),
        "the train must land: {}",
        combined(&landed)
    );
    assert_eq!(rev(&t.fx.repo, "main"), t.tip, "main is the train tip");
    assert_eq!(
        git_ok(
            &t.fx.repo,
            &["status", "--porcelain", "--untracked-files=no"]
        ),
        "",
        "the checked-out base's index and tree follow the fast-forward"
    );
    for mol in [&t.a, &t.b] {
        let st = t.fx.state(mol);
        assert!(!st["merged_at"].is_null(), "{mol} merged_at: {st}");
        assert_eq!(st["archived"], true, "{mol} archived: {st}");
    }
    assert!(t.fx.is_ancestor(&t.ha, "main") && t.fx.is_ancestor(&t.hb, "main"));

    assert_eq!(
        first_parent_shape(&t.fx.repo, &t.base, "main"),
        first_parent_shape(&twin, &t.base, "main"),
        "the landed history must have the shape of two serial `cs done` merges"
    );
    git_ok(&t.fx.repo, &["log", "-1", "--format=%B", "main"])
}

/// A member fast-forwarded into the train instead of merged `--no-ff` puts a
/// worker commit on the first-parent chain, with no merge to carry `Mol-Id`.
#[test]
fn a_member_fast_forwarded_into_the_train_is_refused() {
    let fx = Fixture::new("", false);
    let a = fx.completed_molecule("member a");
    let b = fx.completed_molecule("member b");
    let ha = fx.commit_on_branch(&a, "main", "a.txt", "a\n");
    let base = rev(&fx.repo, "main");
    fx.merge_member_into_train(&a);
    // B is cut from the train and fast-forwarded into it.
    let hb = fx.commit_on_branch(&b, "train", "b.txt", "b\n");
    git_ok(&fx.repo, &["checkout", "-q", "train"]);
    git_ok(
        &fx.repo,
        &["merge", "--ff-only", "-q", &format!("feat/{b}")],
    );
    git_ok(&fx.repo, &["checkout", "-q", "main"]);
    let tip = rev(&fx.repo, "train");
    let t = Train {
        fx,
        a,
        b,
        ha,
        hb: hb.clone(),
        base: base.clone(),
        tip,
    };
    let out = t.land();
    t.assert_refused_naming(&out, &hb, &base);
}

/// A first-parent commit that is not a member merge — here an operator's own
/// commit between the two member merges — is refused and named.
#[test]
fn a_first_parent_commit_that_is_not_a_member_merge_is_refused() {
    let (fx, a, b, ha, hb) = two_members(Fixture::new("", false));
    let base = rev(&fx.repo, "main");
    fx.merge_member_into_train(&a);
    fx.checkout_train();
    fs::write(fx.repo.join("fixup.txt"), "operator\n").unwrap();
    git_ok(&fx.repo, &["add", "fixup.txt"]);
    git_ok(&fx.repo, &["commit", "-qm", "fix the train by hand"]);
    let stray = rev(&fx.repo, "HEAD");
    git_ok(&fx.repo, &["checkout", "-q", "main"]);
    fx.merge_member_into_train(&b);
    let tip = rev(&fx.repo, "train");
    let t = Train {
        fx,
        a,
        b,
        ha,
        hb,
        base: base.clone(),
        tip,
    };
    let out = t.land();
    t.assert_refused_naming(&out, &stray, &base);
}

/// A member merged `--no-ff` without the message `cs done` would write (git's
/// own `Merge branch 'feat/<id>' into train`) is not a member merge.
#[test]
fn a_member_merge_without_the_rendered_message_is_refused() {
    let (fx, a, b, ha, hb) = two_members(Fixture::new("", false));
    let base = rev(&fx.repo, "main");
    fx.merge_member_into_train(&a);
    fx.checkout_train();
    git_ok(
        &fx.repo,
        &["merge", "--no-ff", "-q", "--no-edit", &format!("feat/{b}")],
    );
    let wrong = rev(&fx.repo, "HEAD");
    git_ok(&fx.repo, &["checkout", "-q", "main"]);
    let tip = rev(&fx.repo, "train");
    let t = Train {
        fx,
        a,
        b,
        ha,
        hb,
        base: base.clone(),
        tip,
    };
    let out = t.land();
    t.assert_refused_naming(&out, &wrong, &base);
}

/// A member whose branch moved after it was pinned: what would be closed is
/// not what was gated.
#[test]
fn a_member_whose_branch_moved_after_the_pin_is_refused() {
    let t = happy_train(Fixture::new("", false));
    let moved =
        t.fx.commit_on_branch(&t.b, "main", "after-the-gate.txt", "late\n");
    let out = t.land();
    t.assert_refused_naming(&out, &t.b, &t.base);
    assert!(
        combined(&out).contains(&moved),
        "the refusal names the branch's actual head: {}",
        combined(&out)
    );
}

#[test]
fn a_non_commit_train_revision_names_the_argument_and_leaves_members_open() {
    let t = happy_train(Fixture::new("", false));
    let blob = rev(&t.fx.repo, "main:base.txt");
    let refused = t.fx.land(&t.base, &blob, &t.members());
    t.assert_refused_naming(&refused, "--train-tip", &t.base);
    let text = combined(&refused);
    assert!(text.contains(&blob), "{text}");
    assert!(
        text.contains("does not resolve to a single commit"),
        "{text}"
    );
}

/// The base moved after the train was gated: the compare-and-swap base no
/// longer matches, and the base is left where the other writer put it.
#[test]
fn the_base_moving_after_the_gate_is_refused() {
    let t = happy_train(Fixture::new("", false));
    git_ok(
        &t.fx.repo,
        &["commit", "-q", "--allow-empty", "-m", "someone else landed"],
    );
    let moved = rev(&t.fx.repo, "main");
    let out = t.land();
    t.assert_refused_naming(&out, "main moved after the gate", &moved);
    let text = combined(&out);
    assert!(text.contains("the train was built and gated on"), "{text}");
    assert!(!text.contains("compare-and-swap"), "{text}");
}

#[test]
fn check_leaves_the_repository_and_state_untouched_without_the_trunk_lock() {
    let t = happy_train(Fixture::new("", false));
    let repo = &t.fx.repo;
    fs::write(repo.join("base.txt"), "unrelated local edit\n").unwrap();
    let store = cosmon_filestore::FileStore::new(&repo.join(".cosmon/state"));
    let lock = store.acquire_trunk_lock("test preflight").unwrap();
    let before = file_contents(repo);
    let grants_before = file_contents(&t.fx.trust);
    let check = t.check();
    assert!(check.status.success(), "{}", combined(&check));
    assert!(
        combined(&check).contains("checks passed"),
        "{}",
        combined(&check)
    );
    assert_eq!(
        file_contents(repo),
        before,
        "check changed checkout, Git or Cosmon state"
    );
    assert_eq!(file_contents(&t.fx.trust), grants_before);
    drop(lock);

    let landed = t.land();
    assert!(landed.status.success(), "{}", combined(&landed));
    assert_eq!(rev(repo, "main"), t.tip);
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
    assert_eq!(
        fs::read_to_string(repo.join("base.txt")).unwrap(),
        "unrelated local edit\n"
    );
}

fn file_contents(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(dir: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(&path, files);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, &mut files);
    files
}

#[test]
fn check_revalidates_moved_base_and_member_pins_when_landing() {
    let t = happy_train(Fixture::new("", false));
    let repo = &t.fx.repo;
    let checked = t.check();
    assert!(checked.status.success(), "{}", combined(&checked));
    git_ok(repo, &["commit", "-q", "--allow-empty", "-m", "move base"]);
    t.assert_refused_naming(&t.land(), "main moved after the gate", &rev(repo, "main"));

    git_ok(repo, &["reset", "-q", "--hard", &t.base]);
    let checked = t.check();
    assert!(checked.status.success(), "{}", combined(&checked));
    t.fx.commit_on_branch(&t.b, "main", "b.txt", "moved after check\n");
    t.assert_refused_naming(&t.land(), "not its pinned head", &t.base);
}

#[test]
fn checks_refuse_a_manually_tackled_members_unmerged_blocker_until_it_is_earlier() {
    let fx = Fixture::new("", false);
    let repo = &fx.repo;
    let a = fx.completed_molecule("blocker a");
    let ha = fx.commit_on_branch(&a, "main", "a.txt", "a\n");
    let b = fx.nucleate("dependent b", &["--blocked-by", &a]);
    let socket = format!("test-land-train-{b}");
    let config = repo.join(".cosmon/config.toml");
    fs::write(&config, format!("[project]\nproject_id = \"{socket}\"\n")).unwrap();
    let granted = fx.cs().arg("trust").output().unwrap();
    assert!(granted.status.success(), "{}", combined(&granted));

    let bin = fx._tmp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let claude = bin.join("claude");
    fs::write(
        &claude,
        "#!/bin/sh\ncase \"$1\" in --version|-v) echo 'stub-claude 0.0.0'; exit 0;; esac\n\
         printf '\\342\\235\\257 Type your message\\n'\nexec sleep 600\n",
    )
    .unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let worktree = repo.join(".worktrees").join(&b);
    let tackled = fx
        .cs()
        .env(
            "PATH",
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap(),
        )
        .env("COSMON_CONFIG_HOME", fx._tmp.path().join("config"))
        .env(
            "CLAUDE_CODE_OAUTH_TOKEN",
            "test-placeholder-not-a-credential",
        )
        .args(["tackle", &b, "--adapter", "claude"])
        .output()
        .unwrap();
    // The stub does no work. The fixture commits in the worktree created
    // by manual tackle and completes the molecule through the CLI.
    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();
    assert!(tackled.status.success(), "{}", combined(&tackled));
    assert_eq!(rev(&worktree, "HEAD"), ha);
    fs::write(worktree.join("b.txt"), "b\n").unwrap();
    git_ok(&worktree, &["add", "b.txt"]);
    git_ok(&worktree, &["commit", "-qm", "feat: b"]);
    let hb = rev(&worktree, "HEAD");
    fx.complete(&b);
    assert!(!fx.is_closed(&a));

    let base = rev(repo, "main");
    fx.merge_member_into_train(&b);
    let only_b = rev(repo, "train");
    let members = [(&*b, &*hb)];
    for flags in [&[][..], &["--check"][..]] {
        let out = fx
            .land_command(&base, &only_b, &members)
            .args(flags)
            .output()
            .unwrap();
        let result = combined(&out);
        assert!(
            !out.status.success()
                && result.contains("blocker")
                && result.contains(&a)
                && result.contains(&b),
            "{result}"
        );
        assert_eq!(rev(repo, "main"), base);
        assert!(!fx.is_closed(&a) && !fx.is_closed(&b));
    }

    // B already contains A, so git merge would elide A's trailing merge.
    // Write its two-parent commit explicitly so shape validation can pass.
    let later_a = git_ok(
        repo,
        &[
            "commit-tree",
            &rev(repo, "train^{tree}"),
            "-p",
            &only_b,
            "-p",
            &ha,
            "-m",
            fx.merge_message(&a).trim_end_matches('\n'),
        ],
    );
    let out = fx
        .land_command(&base, &later_a, &[(&b, &hb), (&a, &ha)])
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        !out.status.success() && combined(&out).contains(&format!("blocker {a}")),
        "{}",
        combined(&out)
    );

    git_ok(repo, &["branch", "-f", "train", "main"]);
    fx.merge_member_into_train(&a);
    fx.merge_member_into_train(&b);
    let t = Train {
        tip: rev(repo, "train"),
        fx,
        a,
        b,
        ha,
        hb,
        base,
    };
    let check = t.check();
    assert!(check.status.success(), "{}", combined(&check));
    let land = t.land();
    assert!(land.status.success(), "{}", combined(&land));
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
}

#[test]
fn a_base_move_at_the_compare_and_swap_preserves_the_ref_and_checkout() {
    let t = happy_train(Fixture::new("", false));
    let repo = &t.fx.repo;
    // Make a foreign commit with the same tree, and leave unrelated work on disk.
    let tree = rev(repo, "main^{tree}");
    let foreign = git_ok(
        repo,
        &["commit-tree", &tree, "-p", &t.base, "-m", "foreign"],
    );
    fs::write(repo.join("base.txt"), "local edit\n").unwrap();
    let before = git_ok(repo, &["status", "--porcelain"]);

    // Forward every command to real Git. Just before the train's ref write,
    // verify its index/checkout update has happened and move main once.
    let out = t
        .land_through_git(
            r#"#!/bin/sh
set -eu
export PATH="$LAND_TRAIN_REAL_PATH"
if [ "$#" -ge 6 ] && [ "$3" = update-ref ] && [ "$4" = -m ] && [ "$6" = refs/heads/main ]; then
    test -f "$2/a.txt"
    git -C "$2" diff --cached --name-only | grep -qx a.txt
    git -C "$2" update-ref refs/heads/main "$LAND_TRAIN_FOREIGN" "$LAND_TRAIN_BASE"
fi
exec git "$@"
"#,
        )
        .env("LAND_TRAIN_FOREIGN", &foreign)
        .env("LAND_TRAIN_BASE", &t.base)
        .output()
        .unwrap();

    t.assert_refused_naming(&out, "compare-and-swap", &foreign);
    assert_eq!(git_ok(repo, &["status", "--porcelain"]), before);
    assert_eq!(
        fs::read_to_string(repo.join("base.txt")).unwrap(),
        "local edit\n"
    );
    assert!(!repo.join("a.txt").exists() && !repo.join("b.txt").exists());
}

#[test]
fn a_train_reachable_only_through_a_second_parent_is_not_a_resumable_landing() {
    let t = happy_train(Fixture::new("", false));
    let repo = &t.fx.repo;
    git_ok(repo, &["commit", "-q", "--allow-empty", "-m", "other base"]);
    git_ok(
        repo,
        &["merge", "-q", "--no-ff", "-m", "side merge", "train"],
    );
    let moved = rev(repo, "main");
    assert!(t.fx.is_ancestor(&t.tip, &moved));
    t.assert_refused_naming(&t.land(), "main moved after the gate", &moved);
}

#[test]
fn an_overlapping_checkout_change_prevents_landing() {
    let t = happy_train(Fixture::new("", false));
    let repo = &t.fx.repo;
    fs::write(repo.join("a.txt"), "local work\n").unwrap();

    t.assert_refused_naming(&t.check(), "local changes that overlap", &t.base);
    t.assert_refused_naming(&t.land(), "local changes that overlap", &t.base);
    assert_eq!(
        fs::read_to_string(repo.join("a.txt")).unwrap(),
        "local work\n"
    );
    assert!(!repo.join("b.txt").exists());
    assert_eq!(git_ok(repo, &["status", "--porcelain"]), "?? a.txt");
}

#[test]
fn a_base_checked_out_in_another_worktree_prevents_landing() {
    let t = happy_train(Fixture::new("", false));
    let repo = &t.fx.repo;
    git_ok(repo, &["checkout", "-q", "train"]);
    let checkout = t.fx._tmp.path().join("checkout with a space");
    git_ok(
        repo,
        &["worktree", "add", "-q", checkout.to_str().unwrap(), "main"],
    );

    t.assert_refused_naming(&t.check(), "checkout with a space", &t.base);
    let out = t.land();
    t.assert_refused_naming(&out, "checkout with a space", &t.base);
    assert!(!checkout.join("a.txt").exists());
    assert_eq!(git_ok(&checkout, &["status", "--porcelain"]), "");
    assert_eq!(rev(repo, "HEAD"), t.tip);
}

#[test]
fn landing_from_another_branch_preserves_that_checkout() {
    let t = happy_train(Fixture::new("", false));
    let repo = &t.fx.repo;
    git_ok(repo, &["checkout", "-qb", "operator-draft"]);
    fs::write(repo.join("a.txt"), "operator draft\n").unwrap();

    let out = t.land();
    let text = combined(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("HEAD is on `operator-draft`"), "{text}");
    assert_eq!(rev(repo, "main"), t.tip);
    assert_eq!(
        git_ok(repo, &["symbolic-ref", "--short", "HEAD"]),
        "operator-draft"
    );
    assert_eq!(
        fs::read_to_string(repo.join("a.txt")).unwrap(),
        "operator draft\n"
    );
    assert!(!repo.join("b.txt").exists());
    assert_eq!(git_ok(repo, &["status", "--porcelain"]), "?? a.txt");
    assert!(!t.fx.is_closed(&t.a) && !t.fx.is_closed(&t.b));

    fs::rename(repo.join("a.txt"), repo.join(".cosmon/draft.txt")).unwrap();
    git_ok(repo, &["checkout", "-q", "main"]);
    let resumed = t.land();
    assert!(resumed.status.success(), "{}", combined(&resumed));
    assert_eq!(rev(repo, "main"), t.tip);
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
}

#[test]
fn json_landing_reports_the_members_and_resumption_without_text_progress() {
    let t = happy_train(Fixture::new("", false));
    for (landing, closed, already_closed) in [
        ("landed", vec![&t.a, &t.b], vec![]),
        ("already_landed", vec![], vec![&t.a, &t.b]),
    ] {
        let out =
            t.fx.land_command(&t.base, &t.tip, &t.members())
                .arg("--json")
                .output()
                .unwrap();
        assert!(out.status.success(), "{}", combined(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let reports: Vec<serde_json::Value> = stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{err}: {line}")))
            .collect();
        let (summary, child_reports) = reports.split_last().unwrap();
        assert_eq!(child_reports.len(), closed.len(), "{stdout}");
        for (child, id) in child_reports.iter().zip(&closed) {
            assert_eq!(child["command"], "done", "{stdout}");
            assert_eq!(child["molecule"], id.as_str(), "{stdout}");
        }
        assert_eq!(summary["command"], "land-train", "{stdout}");
        assert_eq!(summary["landing"], landing, "{stdout}");
        assert_eq!(summary["base_branch"], "main", "{stdout}");
        assert_eq!(summary["train_base"], t.base, "{stdout}");
        assert_eq!(summary["train_tip"], t.tip, "{stdout}");
        assert_eq!(summary["closed"], serde_json::json!(closed), "{stdout}");
        assert_eq!(
            summary["already_closed"],
            serde_json::json!(already_closed),
            "{stdout}"
        );
    }
}

/// `[harvest_authority] required = true`: this increment refuses the train.
#[test]
fn a_train_is_refused_when_harvest_authority_is_required() {
    let t = happy_train(Fixture::new(
        "[harvest_authority]\nrequired = true\n",
        false,
    ));
    t.assert_refused_naming(&t.check(), "harvest_authority", &t.base);
    let out = t.land();
    t.assert_refused_naming(&out, "harvest_authority", &t.base);
}

/// A member that changes the trusted shell surface, with no trust decision
/// covering the surface the train would leave on disk: refused, the path is
/// named, and no trust is granted. Following the printed remedy (trust the
/// train tip's surface, rerun) then lands the train.
#[test]
fn a_train_changing_the_shell_surface_without_trust_is_refused() {
    let fx = Fixture::new("", true);
    // Trust the base surface, as an operator running this galaxy would have.
    let granted = fx.cs().arg("trust").output().unwrap();
    assert!(granted.status.success(), "{}", combined(&granted));
    let a = fx.completed_molecule("member a");
    let b = fx.completed_molecule("member b");
    let mut config = fs::read_to_string(fx.repo.join(".cosmon/config.toml")).unwrap();
    config.push_str("# changed by member a\n");
    let ha = fx.commit_on_branch(&a, "main", ".cosmon/config.toml", &config);
    let hb = fx.commit_on_branch(&b, "main", "b.txt", "b\n");
    let base = rev(&fx.repo, "main");
    fx.merge_member_into_train(&a);
    fx.merge_member_into_train(&b);
    let tip = rev(&fx.repo, "train");
    let t = Train {
        fx,
        a,
        b,
        ha,
        hb,
        base: base.clone(),
        tip: tip.clone(),
    };
    let grants_before = trust_store_listing(&t.fx.trust);

    t.assert_refused_naming(&t.check(), ".cosmon/config.toml", &base);
    let out = t.land();
    t.assert_refused_naming(&out, ".cosmon/config.toml", &base);
    assert_eq!(
        trust_store_listing(&t.fx.trust),
        grants_before,
        "cs land-train never grants trust"
    );

    // The remedy the refusal prints: trust the surface the train would leave
    // on disk, from a checkout of the train tip, then rerun.
    let text = combined(&out);
    assert!(text.contains("cs trust"), "remedy names cs trust: {text}");
    git_ok(&t.fx.repo, &["checkout", "-q", "--detach", &tip]);
    let trusted = t.fx.cs().arg("trust").output().unwrap();
    assert!(trusted.status.success(), "{}", combined(&trusted));
    git_ok(&t.fx.repo, &["checkout", "-q", "main"]);

    let checked = t.check();
    assert!(checked.status.success(), "{}", combined(&checked));
    let landed = t.land();
    assert!(
        landed.status.success(),
        "after the remedy the train lands: {}",
        combined(&landed)
    );
    assert_eq!(rev(&t.fx.repo, "main"), tip);
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
}

#[test]
fn a_train_already_on_the_upstream_base_closes_after_sync_under_stale_trust() {
    const SCRIPT: &str = "tools/ci/check-build.sh";
    let fx = Fixture::new(
        "[gates]\nbuild_command = 'sh tools/ci/check-build.sh'\n",
        true,
    );
    fs::create_dir_all(fx.repo.join("tools/ci")).unwrap();
    fs::write(fx.repo.join(SCRIPT), "exit 0\n").unwrap();
    git_ok(&fx.repo, &["add", SCRIPT]);
    git_ok(&fx.repo, &["commit", "-qm", "add the build gate"]);
    let a = fx.completed_molecule("member a");
    let b = fx.completed_molecule("member b");
    let cards = fx
        .repo
        .join(".cosmon/state/fleets/default/molecules")
        .join(&a);
    let card_names = ["intent-card.md", "mission-card.md"];
    for card in card_names {
        fs::write(cards.join(card), "# Original card\n").unwrap();
        git_ok(&fx.repo, &["add", "-f", cards.join(card).to_str().unwrap()]);
    }
    git_ok(&fx.repo, &["commit", "-qm", "track the molecule cards"]);
    let base = rev(&fx.repo, "main");
    let granted = fx.cs().arg("trust").output().unwrap();
    assert!(granted.status.success(), "{}", combined(&granted));

    git_ok(&fx.repo, &["checkout", "-qb", &format!("feat/{a}")]);
    for card in card_names {
        fs::write(cards.join(card), "# Updated card\n").unwrap();
        git_ok(&fx.repo, &["add", "-f", cards.join(card).to_str().unwrap()]);
    }
    git_ok(&fx.repo, &["commit", "-qm", "update molecule cards"]);
    assert_eq!(trust_status(&fx), "trusted", "card edits alone keep trust");
    git_ok(&fx.repo, &["checkout", "-q", "main"]);
    let ha = fx.commit_on_branch(&a, &base, SCRIPT, "touch ../gate-ran\n");
    let hb = fx.commit_on_branch(&b, &base, "b.txt", "b\n");
    fx.merge_member_into_train(&a);
    fx.merge_member_into_train(&b);
    let tip = rev(&fx.repo, "train");
    let changed = git_ok(&fx.repo, &["diff", "--name-only", &base, &tip]);
    for card in card_names {
        let path = cards.join(card);
        let relative = path.strip_prefix(&fx.repo).unwrap().to_str().unwrap();
        assert!(changed.lines().any(|line| line == relative), "{changed}");
    }
    let marker = fx._tmp.path().join("gate-ran");
    fx.checkout_train();
    assert!(Command::new("sh")
        .arg(SCRIPT)
        .current_dir(&fx.repo)
        .status()
        .unwrap()
        .success());
    fs::remove_file(&marker).unwrap();
    git_ok(&fx.repo, &["checkout", "-q", "main"]);

    // Model the published base with a local bare upstream. The operator's
    // local base is still at the original gate input until they pull it.
    let upstream = fx._tmp.path().join("upstream.git");
    git_ok(
        &fx.repo,
        &["clone", "-q", "--bare", ".", upstream.to_str().unwrap()],
    );
    git_ok(
        &fx.repo,
        &["remote", "add", "origin", upstream.to_str().unwrap()],
    );
    git_ok(&fx.repo, &["fetch", "-q", "origin"]);
    git_ok(
        &fx.repo,
        &["branch", "--set-upstream-to=origin/main", "main"],
    );
    git_ok(&upstream, &["update-ref", "refs/heads/main", &tip]);
    let t = Train {
        fx,
        a,
        b,
        ha,
        hb,
        base,
        tip,
    };
    assert_eq!(rev(&upstream, "main"), t.tip);
    assert_eq!(rev(&t.fx.repo, "main"), t.base);
    assert_eq!(rev(&t.fx.repo, "origin/main"), t.base);
    let refused = t.land();
    t.assert_refused_naming(&refused, SCRIPT, &t.base);
    let refusal = combined(&refused);
    assert!(
        !refusal.contains("intent-card.md") && !refusal.contains("mission-card.md"),
        "{refusal}"
    );

    git_ok(&t.fx.repo, &["pull", "--ff-only"]);
    assert_eq!(rev(&t.fx.repo, "main"), t.tip);
    assert_eq!(trust_status(&t.fx), "stale");
    let grants_before = trust_store_listing(&t.fx.trust);

    let closed = t.land();
    let text = combined(&closed);
    assert!(
        closed.status.success(),
        "already-landed closure failed: {text}"
    );
    let warnings: Vec<_> = text
        .lines()
        .filter(|line| line.contains("trusted shell surface changed"))
        .collect();
    assert!(warnings.iter().any(|line| line.contains(SCRIPT)), "{text}");
    assert!(
        warnings
            .iter()
            .all(|line| { !card_names.iter().any(|card| line.contains(card)) }),
        "{text}"
    );
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
    assert_eq!(rev(&t.fx.repo, "main"), t.tip);
    assert_eq!(trust_store_listing(&t.fx.trust), grants_before);
    assert!(!marker.exists(), "closure must skip the build gate");

    assert!(
        refusal.contains("git pull --ff-only"),
        "the refusal must tell an operator whose train was already published how to resume: {refusal}"
    );
}

fn trust_status(fx: &Fixture) -> String {
    let out = fx
        .cs()
        .args(["--json", "trust", "--status"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", combined(&out));
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    status["status"].as_str().unwrap().to_owned()
}

fn trust_store_listing(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read_to_string(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

/// A kill between the fast-forward and the first closure leaves the base at
/// the train tip with no member closed. The rerun skips the landing and
/// closes every member exactly once.
#[test]
fn a_rerun_after_a_kill_before_any_closure_closes_each_member_once() {
    let t = happy_train(Fixture::new("", false));
    // Let Git publish the ref, then kill the waiting cs before the Git child returns.
    let interrupted = t
        .land_through_git(
            r#"#!/bin/sh
set -eu
export PATH="$LAND_TRAIN_REAL_PATH"
if [ "$#" -ge 6 ] && [ "$3" = update-ref ] && [ "$4" = -m ] && [ "$6" = refs/heads/main ]; then
    git "$@"
    kill -KILL "$PPID"
    exit 0
fi
exec git "$@"
"#,
        )
        .output()
        .unwrap();
    assert_eq!(
        interrupted.status.signal(),
        Some(9),
        "{}",
        combined(&interrupted)
    );
    assert_eq!(rev(&t.fx.repo, "main"), t.tip);
    assert!(!t.fx.is_closed(&t.a) && !t.fx.is_closed(&t.b));

    let rerun = t.land();
    assert!(
        rerun.status.success(),
        "the rerun must finish the closures: {}",
        combined(&rerun)
    );
    assert_eq!(
        rev(&t.fx.repo, "main"),
        t.tip,
        "the base is not moved again"
    );
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
}

/// A kill after the first member was closed: the rerun leaves that member
/// alone (its `merged_at` is unchanged, it is not closed a second time) and
/// closes the rest.
#[test]
fn a_rerun_after_a_kill_between_closures_does_not_close_a_member_twice() {
    let t = happy_train(Fixture::new("", false));
    git_ok(&t.fx.repo, &["merge", "--ff-only", "-q", &t.tip]);
    let first =
        t.fx.cs()
            .args(["done", &t.a, "--no-auto-propel", "--expect-head", &t.ha])
            .output()
            .unwrap();
    assert!(first.status.success(), "{}", combined(&first));
    let a_merged_at = t.fx.state(&t.a)["merged_at"].clone();
    assert!(!a_merged_at.is_null());

    let rerun = t.land();
    assert!(
        rerun.status.success(),
        "the rerun must finish the closures: {}",
        combined(&rerun)
    );
    assert_eq!(rev(&t.fx.repo, "main"), t.tip);
    assert_eq!(
        t.fx.state(&t.a)["merged_at"],
        a_merged_at,
        "member A was closed once; its merged_at must not be rewritten"
    );
    assert!(t.fx.is_closed(&t.b), "member B is closed by the rerun");
}

/// In a galaxy that tracks harvest state, closing member A commits that state
/// to the base, which then sits past the train tip. A rerun after a kill at
/// that point must still read the train as landed — the tip is on the base's
/// first-parent chain — and close only member B.
#[test]
fn a_rerun_after_a_closure_committed_tracked_state_closes_the_rest() {
    let t = happy_train(Fixture::new_tracked_state());
    git_ok(&t.fx.repo, &["merge", "--ff-only", "-q", &t.tip]);
    let first =
        t.fx.cs()
            .args(["done", &t.a, "--no-auto-propel", "--expect-head", &t.ha])
            .output()
            .unwrap();
    assert!(first.status.success(), "{}", combined(&first));
    let a_merged_at = t.fx.state(&t.a)["merged_at"].clone();

    let rerun = t.land();
    assert!(
        rerun.status.success(),
        "the rerun must finish the closures: {}",
        combined(&rerun)
    );
    assert!(t.fx.is_ancestor(&t.tip, "main"));
    assert_eq!(t.fx.state(&t.a)["merged_at"], a_merged_at);
    assert!(t.fx.is_closed(&t.b), "member B is closed by the rerun");
}

/// The same galaxy, landed in one run: each closure may commit tracked state
/// to the base, and the next closure must not be disturbed by it.
#[test]
fn a_train_lands_in_a_galaxy_that_tracks_harvest_state() {
    let t = happy_train(Fixture::new_tracked_state());
    let landed = t.land();
    assert!(
        landed.status.success(),
        "the train must land: {}",
        combined(&landed)
    );
    assert!(t.fx.is_ancestor(&t.tip, "main"));
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
}

/// The case the tip extraction exists for: the base's config already names a
/// script that does not exist yet, and a member adds it. The script is in the
/// surface only at the tip, so a check against the base on disk would miss it.
#[test]
fn a_member_adding_a_script_the_config_already_names_is_refused() {
    let fx = Fixture::new("[gates]\nbuild_command = \"sh scripts/build.sh\"\n", true);
    let granted = fx.cs().arg("trust").output().unwrap();
    assert!(granted.status.success(), "{}", combined(&granted));
    let a = fx.completed_molecule("member a");
    let b = fx.completed_molecule("member b");
    let ha = fx.commit_on_branch(&a, "main", "scripts/build.sh", "exit 0\n");
    let hb = fx.commit_on_branch(&b, "main", "b.txt", "b\n");
    let base = rev(&fx.repo, "main");
    fx.merge_member_into_train(&a);
    fx.merge_member_into_train(&b);
    let tip = rev(&fx.repo, "train");
    let t = Train {
        fx,
        a,
        b,
        ha,
        hb,
        base: base.clone(),
        tip,
    };
    let out = t.land();
    t.assert_refused_naming(&out, "scripts/build.sh", &base);
}

/// The first closure that fails stops the run: the base has moved, the
/// members already closed and the ones still open are named, and a rerun
/// closes the rest. Driven through the verb by a `pre_done` hook that fails
/// on its second call only.
#[test]
fn a_failed_closure_stops_the_run_and_a_rerun_closes_the_rest() {
    let tmp_hook = tempfile::tempdir().unwrap();
    let script = tmp_hook.path().join("hook.sh");
    let count = tmp_hook.path().join("count");
    fs::write(
        &script,
        format!(
            "n=$(cat '{c}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{c}'; \
             [ \"$n\" -eq 2 ] && exit 1; exit 0\n",
            c = count.display()
        ),
    )
    .unwrap();
    let t = happy_train(Fixture::new(
        &format!("[hooks]\npre_done = \"sh {}\"\n", script.display()),
        false,
    ));
    let granted = t.fx.cs().arg("trust").output().unwrap();
    assert!(granted.status.success(), "{}", combined(&granted));

    let out = t.land();
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "the failed closure must fail the run: {text}"
    );
    assert_eq!(
        rev(&t.fx.repo, "main"),
        t.tip,
        "the base moved before the closures"
    );
    assert!(t.fx.is_closed(&t.a), "member A was closed: {text}");
    assert!(!t.fx.is_closed(&t.b), "member B was not: {text}");
    assert!(
        text.contains(&format!("Closed: [{}]", t.a))
            && text.contains(&format!("Not closed: [{}]", t.b)),
        "the report names closed and open members: {text}"
    );
    let a_merged_at = t.fx.state(&t.a)["merged_at"].clone();

    let rerun = t.land();
    assert!(
        rerun.status.success(),
        "the rerun closes the rest: {}",
        combined(&rerun)
    );
    assert!(t.fx.is_closed(&t.b));
    assert_eq!(t.fx.state(&t.a)["merged_at"], a_merged_at);
}

/// A collapsed member is refused before the base moves: `cs done` would close
/// a Collapsed molecule, so the landing must check Completed itself
/// (ADR-052 §I9).
#[test]
fn a_collapsed_member_is_refused() {
    let fx = Fixture::new("", false);
    let a = fx.completed_molecule("member a");
    let out = fx
        .cs()
        .args(["--json", "nucleate", "task-work", "--var", "topic=member b"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", combined(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let b = v["id"].as_str().unwrap().to_owned();
    let col = fx
        .cs()
        .args(["collapse", &b, "--reason", "abandoned"])
        .output()
        .unwrap();
    assert!(col.status.success(), "{}", combined(&col));
    let ha = fx.commit_on_branch(&a, "main", "a.txt", "a\n");
    let hb = fx.commit_on_branch(&b, "main", "b.txt", "b\n");
    let base = rev(&fx.repo, "main");
    fx.merge_member_into_train(&a);
    fx.merge_member_into_train(&b);
    let tip = rev(&fx.repo, "train");
    let t = Train {
        fx,
        a,
        b: b.clone(),
        ha,
        hb,
        base: base.clone(),
        tip,
    };
    let out = t.land();
    t.assert_refused_naming(&out, &format!("member {b} is collapsed"), &base);
}

/// The worker of a member cannot land the train: the guard `cs done` runs
/// first is run per member before the base moves, not left to the closures.
#[test]
fn a_member_worker_cannot_land_the_train() {
    let t = happy_train(Fixture::new("", false));
    let mut cmd = t.fx.cs();
    cmd.env("COSMON_PARENT_MOL_ID", &t.b).args([
        "land-train",
        "--train-base",
        &t.base,
        "--train-tip",
        &t.tip,
    ]);
    for (mol, head) in t.members() {
        cmd.arg("--member").arg(format!("{mol}={head}"));
    }
    let out = cmd.output().unwrap();
    t.assert_refused_naming(&out, "harvest belongs to the pilot", &t.base);
}
