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
        let out = self
            .cs()
            .args(["--json", "nucleate", "task-work", "--var"])
            .arg(format!("topic={topic}"))
            .output()
            .expect("cs nucleate");
        assert!(out.status.success(), "nucleate failed: {}", combined(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let mol_id = v["id"].as_str().expect("nucleate id").to_owned();
        let done = self
            .cs()
            .args([
                "complete",
                &mol_id,
                "--reason",
                "test",
                "--ignore-mindguard",
            ])
            .output()
            .expect("cs complete");
        assert!(
            done.status.success(),
            "complete failed: {}",
            combined(&done)
        );
        mol_id
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

    let out = t.land();
    t.assert_refused_naming(&out, "checkout with a space", &t.base);
    assert!(!checkout.join("a.txt").exists());
    assert_eq!(git_ok(&checkout, &["status", "--porcelain"]), "");
    assert_eq!(rev(repo, "HEAD"), t.tip);
}

/// `[harvest_authority] required = true`: this increment refuses the train.
#[test]
fn a_train_is_refused_when_harvest_authority_is_required() {
    let t = happy_train(Fixture::new(
        "[harvest_authority]\nrequired = true\n",
        false,
    ));
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

    let landed = t.land();
    assert!(
        landed.status.success(),
        "after the remedy the train lands: {}",
        combined(&landed)
    );
    assert_eq!(rev(&t.fx.repo, "main"), tip);
    assert!(t.fx.is_closed(&t.a) && t.fx.is_closed(&t.b));
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
