// SPDX-License-Identifier: AGPL-3.0-only

//! Issues #215 and #213: closing a molecule whose branch was already landed on
//! the base outside `cs done` runs no post-merge gate, needs no shell trust,
//! and warns about trusted-shell-surface paths that reached the base unreviewed.
//!
//! "No merge, nothing to verify" (ADR-158 amendment): the gate verifies a tree
//! `cs done` produced. When `cs done` finds the branch already reachable from
//! the base and moves nothing, there is no such tree.
//!
//! Every test drives the real `cs` binary against a real git repository and a
//! real trust store. The gate commands write a marker file when run, so "ran no
//! gate" is observed on disk, not inferred from an exit code.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cs(repo: &Path, trust_store: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    apply_env(&mut cmd, repo, trust_store);
    cmd.current_dir(repo.join(".cosmon/state"));
    cmd
}

fn apply_env(cmd: &mut Command, repo: &Path, trust_store: &Path) {
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_SKIP_PRE_DONE_HOOK")
        .env_remove("COSMON_ASSUME_TRUSTED")
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env("COSMON_TRUST_DIR", trust_store)
        .env("GIT_PAGER", "cat");
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

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn write_file(repo: &Path, path: &str, contents: &str) {
    let full = repo.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, contents).unwrap();
}

/// A repository whose config, formulas and gate scripts are tracked, so the
/// shell surface can change through ordinary commits.
fn setup_repo(repo: &Path, config: &str, files: &[(&str, &str)]) {
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "Test"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(cosmon.join("config.toml"), config).unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(repo.join(".gitignore"), ".cosmon/state/\n.worktrees/\n").unwrap();
    for (path, contents) in files {
        write_file(repo, path, contents);
    }
}

fn nucleate_terminal(repo: &Path, trust_store: &Path) -> String {
    let nuc = cs(repo, trust_store)
        .args(["--json", "nucleate", "task-work", "--var", "topic=landed"])
        .output()
        .expect("cs nucleate");
    assert!(nuc.status.success(), "nucleate failed: {}", combined(&nuc));
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();
    let col = cs(repo, trust_store)
        .args(["--json", "collapse", &mol_id, "--reason", "fixture"])
        .output()
        .expect("cs collapse");
    assert!(col.status.success(), "collapse failed: {}", combined(&col));
    mol_id
}

/// Commit everything on main as the base, then create `feat/<mol_id>` holding
/// `changes`. Main stays checked out.
fn stage_branch(repo: &Path, mol_id: &str, changes: &[(&str, &str)]) -> String {
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-qm", "base"]);
    let branch = format!("feat/{mol_id}");
    git_ok(repo, &["checkout", "-q", "-b", &branch]);
    for (path, contents) in changes {
        write_file(repo, path, contents);
    }
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-qm", "worker change"]);
    git_ok(repo, &["checkout", "-q", "main"]);
    branch
}

fn grant_trust(repo: &Path, trust_store: &Path) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    apply_env(&mut cmd, repo, trust_store);
    let out = cmd
        .current_dir(repo)
        .arg("trust")
        .output()
        .expect("cs trust");
    assert!(out.status.success(), "cs trust failed: {}", combined(&out));
}

fn trust_status(repo: &Path, trust_store: &Path) -> String {
    combined(
        &cs(repo, trust_store)
            .args(["trust", "--status"])
            .output()
            .unwrap(),
    )
}

fn done(repo: &Path, trust_store: &Path, mol_id: &str) -> Output {
    cs(repo, trust_store)
        .args(["done", mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done")
}

fn done_json(repo: &Path, trust_store: &Path, mol_id: &str) -> Output {
    cs(repo, trust_store)
        .args(["--json", "done", mol_id, "--no-auto-propel"])
        .output()
        .expect("cs --json done")
}

fn molecule_state(repo: &Path, mol_id: &str) -> serde_json::Value {
    let path = repo
        .join(".cosmon/state/fleets/default/molecules")
        .join(mol_id)
        .join("state.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["rev-parse", "--verify", branch])
        .status
        .success()
}

/// How the branch reaches the base outside `cs done`.
#[derive(Clone, Copy)]
enum Landing {
    NoFf,
    FastForward,
}

fn land_by_hand(repo: &Path, branch: &str, how: Landing) {
    match how {
        Landing::NoFf => git_ok(
            repo,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "land the branch by hand",
                branch,
            ],
        ),
        Landing::FastForward => git_ok(repo, &["merge", "-q", "--ff-only", branch]),
    }
}

/// Config declaring a build gate that would FAIL, and leave `marker` if it ran.
/// The script is a delegated target, so it is part of the trusted shell surface.
fn failing_gate_config(marker: &Path) -> (String, String) {
    let config =
        "[project]\nproject_id = \"issue-74\"\n\n[gates]\nbuild_command = 'sh scripts/build.sh'\n"
            .to_owned();
    let script = format!("touch '{}'\nexit 1\n", marker.display());
    (config, script)
}

/// Make the recorded trust stale WITHOUT touching anything inside the landed
/// range: a later commit on the base edits the gate script.
fn make_trust_stale(repo: &Path, trust_store: &Path, script_body: &str) {
    write_file(
        repo,
        "scripts/build.sh",
        &format!("{script_body}# touched after the landing\n"),
    );
    git_ok(repo, &["add", "-A"]);
    git_ok(
        repo,
        &["commit", "-qm", "edit the gate script after the landing"],
    );
    assert!(
        trust_status(repo, trust_store).contains("stale"),
        "fixture must leave trust stale: {}",
        trust_status(repo, trust_store)
    );
}

struct Fixture {
    _tmp: tempfile::TempDir,
    _store: tempfile::TempDir,
    repo: PathBuf,
    store: PathBuf,
    marker: PathBuf,
    mol_id: String,
    branch: String,
}

/// Branch staged and trusted, then landed by hand, then trust made stale.
/// `branch_changes` is what the worker commits; `landing` how it reaches main.
fn landed_fixture(landing: Landing, branch_changes: &[(&str, &str)], stale: bool) -> Fixture {
    landed_fixture_with(landing, branch_changes, stale, |_, _| {})
}

/// [`landed_fixture`] with a step between staging the branch and landing it,
/// given the repository and the branch name.
fn landed_fixture_with(
    landing: Landing,
    branch_changes: &[(&str, &str)],
    stale: bool,
    before_landing: impl FnOnce(&Path, &str),
) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    let marker = repo.join("gate-ran.marker");
    let (config, script) = failing_gate_config(&marker);
    setup_repo(
        &repo,
        &config,
        &[
            ("scripts/build.sh", &script),
            ("src/main.py", "print('hi')\n"),
        ],
    );
    let mol_id = nucleate_terminal(&repo, store.path());
    let branch = stage_branch(&repo, &mol_id, branch_changes);
    grant_trust(&repo, store.path());
    before_landing(&repo, &branch);
    land_by_hand(&repo, &branch, landing);
    if stale {
        make_trust_stale(&repo, store.path(), &script);
    }
    let store_path = store.path().to_path_buf();
    Fixture {
        _tmp: tmp,
        _store: store,
        repo,
        store: store_path,
        marker,
        mol_id,
        branch,
    }
}

fn assert_closed_without_gate(fx: &Fixture, out: &Output) {
    let text = combined(out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("already_merged"), "{text}");
    assert!(
        !text.contains("rolled back") && !text.contains("GATE REFUSED"),
        "no refusal wording when nothing was merged:\n{text}"
    );
    assert!(
        !fx.marker.exists(),
        "the declared gate command ran although cs done merged nothing:\n{text}"
    );
    assert!(
        !molecule_state(&fx.repo, &fx.mol_id)["merged_at"].is_null(),
        "merged_at must be stamped from ancestry:\n{text}"
    );
    assert!(
        !branch_exists(&fx.repo, &fx.branch),
        "teardown must delete the landed branch:\n{text}"
    );
    // Trust is not touched on behalf of a gate that did not run.
    assert!(
        trust_status(&fx.repo, &fx.store).contains("stale"),
        "closure must not change the trust grant"
    );
}

/// `--no-ff` landing outside `cs done`, trust stale, gate that would fail:
/// exits 0, reports `already_merged`, stamps `merged_at`, runs no gate command.
#[test]
fn closing_a_no_ff_landed_branch_runs_no_gate_and_needs_no_trust() {
    let fx = landed_fixture(Landing::NoFf, &[("src/work.py", "print('w')\n")], true);
    let out = done(&fx.repo, &fx.store, &fx.mol_id);
    assert_closed_without_gate(&fx, &out);
}

/// Same, for a fast-forward landing.
#[test]
fn closing_a_fast_forwarded_branch_runs_no_gate_and_needs_no_trust() {
    let fx = landed_fixture(
        Landing::FastForward,
        &[("src/work.py", "print('w')\n")],
        true,
    );
    let out = done(&fx.repo, &fx.store, &fx.mol_id);
    assert_closed_without_gate(&fx, &out);
}

/// The landing commit is recorded as evidence in the same place the existing
/// evidence goes: the `actions` list of the `--json` report.
#[test]
fn closure_records_the_landing_commit_as_evidence() {
    let fx = landed_fixture(Landing::NoFf, &[("src/work.py", "print('w')\n")], true);
    let landing = String::from_utf8_lossy(
        &git(
            &fx.repo,
            &["rev-list", "--first-parent", "--merges", "-n1", "main"],
        )
        .stdout,
    )
    .trim()
    .to_owned();
    assert_eq!(landing.len(), 40, "fixture must have a merge commit");
    // The merge commit is no longer HEAD (a later commit made trust stale).
    let out = done_json(&fx.repo, &fx.store, &fx.mol_id);
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains(&format!("landed_outside_cs_done: {landing}")),
        "the landing commit {landing} must be named in the actions:\n{text}"
    );
}

/// A landing that changed a shell-surface file: the warning names that path and
/// says the change reached the base outside `cs done`, unreviewed.
#[test]
fn closure_warns_about_a_surface_path_the_landing_changed() {
    let fx = landed_fixture(
        Landing::NoFf,
        &[("scripts/build.sh", "touch ignored\nexit 1\n")],
        false,
    );
    // The branch itself rewrote the gate script; trust is already stale.
    let out = done(&fx.repo, &fx.store, &fx.mol_id);
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("scripts/build.sh"),
        "warning must name the surface path:\n{text}"
    );
    assert!(
        text.contains("outside `cs done`") && text.contains("not reviewed"),
        "warning must say the change bypassed cs done's review:\n{text}"
    );
}

/// Same warning for a fast-forward landing, where the fork point comes from the
/// branch reflog.
#[test]
fn closure_warns_about_a_surface_path_a_fast_forward_changed() {
    let fx = landed_fixture(
        Landing::FastForward,
        &[("scripts/build.sh", "touch ignored\nexit 1\n")],
        false,
    );
    let out = done(&fx.repo, &fx.store, &fx.mol_id);
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("scripts/build.sh"), "{text}");
    assert!(text.contains("not reviewed"), "{text}");
}

/// A landing that changed no surface path: no warning, even though a LATER
/// commit (outside the landed range) made the trust stale.
#[test]
fn closure_does_not_warn_when_the_landing_changed_no_surface_path() {
    for landing in [Landing::NoFf, Landing::FastForward] {
        let fx = landed_fixture(landing, &[("src/work.py", "print('w')\n")], true);
        let out = done(&fx.repo, &fx.store, &fx.mol_id);
        let text = combined(&out);
        assert_eq!(out.status.code(), Some(0), "{text}");
        assert!(
            !text.contains("not reviewed") && !text.contains("shell surface"),
            "no warning when no surface path changed:\n{text}"
        );
        assert!(
            !text.contains("scripts/build.sh"),
            "a surface edit outside the landed range must not be reported:\n{text}"
        );
    }
}

/// Unchanged behaviour: a real merge performed by `cs done` still runs the gate
/// and still refuses under stale trust.
#[test]
fn a_real_merge_by_cs_done_still_runs_the_gate_and_refuses_under_stale_trust() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let marker = repo.join("gate-ran.marker");
    let (config, script) = failing_gate_config(&marker);
    setup_repo(
        repo,
        &config,
        &[
            ("scripts/build.sh", &script),
            ("src/main.py", "print('hi')\n"),
        ],
    );
    let mol_id = nucleate_terminal(repo, store.path());
    let branch = stage_branch(repo, &mol_id, &[("src/work.py", "print('w')\n")]);
    grant_trust(repo, store.path());
    make_trust_stale(repo, store.path(), &script);
    let main_before = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();

    let out = done(repo, store.path(), &mol_id);
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("POST-MERGE COMPILE GATE REFUSED") && text.contains("trust is stale"),
        "{text}"
    );
    assert!(
        text.contains("merge rolled back"),
        "a real merge keeps the rollback wording:\n{text}"
    );
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(main_before, main_after, "refused merge must be rolled back");
    assert!(branch_exists(repo, &branch));
    assert!(!marker.exists(), "untrusted gate must not run");
}

/// A real merge with fresh trust runs the gate (marker appears) — the guard that
/// the new short-circuit is scoped to `AlreadyMerged`, not to every merge.
#[test]
fn a_real_merge_by_cs_done_with_fresh_trust_runs_the_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let marker = repo.join("gate-ran.marker");
    let (config, script) = failing_gate_config(&marker);
    setup_repo(
        repo,
        &config,
        &[
            ("scripts/build.sh", &script),
            ("src/main.py", "print('hi')\n"),
        ],
    );
    let mol_id = nucleate_terminal(repo, store.path());
    let _ = stage_branch(repo, &mol_id, &[("src/work.py", "print('w')\n")]);
    grant_trust(repo, store.path());
    let out = done(repo, store.path(), &mol_id);
    assert_eq!(out.status.code(), Some(1), "{}", combined(&out));
    assert!(marker.exists(), "the gate must run on a real merge");
}

/// The `post_merge` hook deploys. It runs after a merge `cs done` performed and
/// does not run when `cs done` merged nothing.
#[test]
fn post_merge_hook_runs_after_a_real_merge_and_not_after_an_already_landed_closure() {
    let post_marker = |repo: &Path| repo.join("deployed.marker");
    let config = |repo: &Path| {
        format!(
            "[project]\nproject_id = \"issue-74\"\n\n[hooks]\npost_merge = \"touch '{}'\"\n",
            post_marker(repo).display()
        )
    };

    // Real merge by `cs done`: the hook runs.
    {
        let tmp = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_repo(repo, &config(repo), &[("src/main.py", "print('hi')\n")]);
        let mol_id = nucleate_terminal(repo, store.path());
        let _ = stage_branch(repo, &mol_id, &[("src/work.py", "print('w')\n")]);
        grant_trust(repo, store.path());
        let out = done(repo, store.path(), &mol_id);
        assert_eq!(out.status.code(), Some(0), "{}", combined(&out));
        assert!(
            post_marker(repo).exists(),
            "post_merge must run after a real merge:\n{}",
            combined(&out)
        );
    }

    // Already landed outside `cs done`: the hook does not run, trust is fresh.
    {
        let tmp = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_repo(repo, &config(repo), &[("src/main.py", "print('hi')\n")]);
        let mol_id = nucleate_terminal(repo, store.path());
        let branch = stage_branch(repo, &mol_id, &[("src/work.py", "print('w')\n")]);
        grant_trust(repo, store.path());
        land_by_hand(repo, &branch, Landing::NoFf);
        let out = done(repo, store.path(), &mol_id);
        let text = combined(&out);
        assert_eq!(out.status.code(), Some(0), "{text}");
        assert!(text.contains("already_merged"), "{text}");
        assert!(
            !post_marker(repo).exists(),
            "post_merge deployed although cs done merged nothing:\n{text}"
        );
    }
}

/// `pre_done` is configured shell that WOULD run, so it legitimately needs
/// trust. Under stale trust the refusal names the hook, says why trust is
/// needed, gives `cs trust` then rerun, and never claims a rollback.
#[test]
fn pre_done_under_stale_trust_is_refused_with_its_own_wording() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let marker = repo.join("pre-done-ran.marker");
    let config = format!(
        "[project]\nproject_id = \"issue-74\"\n\n[hooks]\npre_done = \"sh scripts/pre_done.sh\"\n"
    );
    let script = format!("touch '{}'\n", marker.display());
    setup_repo(
        repo,
        &config,
        &[
            ("scripts/pre_done.sh", &script),
            ("src/main.py", "print('hi')\n"),
        ],
    );
    let mol_id = nucleate_terminal(repo, store.path());
    let branch = stage_branch(repo, &mol_id, &[("src/work.py", "print('w')\n")]);
    grant_trust(repo, store.path());
    land_by_hand(repo, &branch, Landing::NoFf);
    write_file(repo, "scripts/pre_done.sh", &format!("{script}# edited\n"));
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-qm", "edit the pre_done script"]);

    let out = done(repo, store.path(), &mol_id);
    let text = combined(&out);
    assert_ne!(out.status.code(), Some(0), "{text}");
    assert!(!marker.exists(), "pre_done must not run under stale trust");
    assert!(
        text.contains("pre_done"),
        "the refusal must name the hook:\n{text}"
    );
    assert!(
        text.contains("cs trust") && text.contains("rerun"),
        "the refusal must give `cs trust` then rerun:\n{text}"
    );
    assert!(
        !text.contains("rolled back"),
        "nothing was merged, so no rollback wording:\n{text}"
    );
    assert!(
        molecule_state(repo, &mol_id)["merged_at"].is_null(),
        "a refused closure stamps nothing:\n{text}"
    );
}

/// The branch's reflog is the first choice for the fork point. Without it a
/// `--no-ff` landing still has a fork point: the merge base of the branch tip
/// with the landing commit's first parent. The warning says which evidence it
/// used.
#[test]
fn closure_falls_back_to_the_merge_base_when_the_reflog_is_gone() {
    let fx = landed_fixture_with(
        Landing::NoFf,
        &[("scripts/build.sh", "touch ignored\nexit 1\n")],
        false,
        |repo, branch| {
            let log = repo.join(".git/logs/refs/heads").join(branch);
            assert!(log.exists(), "fixture assumes git wrote a branch reflog");
            fs::remove_file(log).unwrap();
        },
    );
    let out = done(&fx.repo, &fx.store, &fx.mol_id);
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("scripts/build.sh"), "{text}");
    assert!(
        text.contains("merge base with the pre-landing base"),
        "the warning must name the evidence the fork point came from:\n{text}"
    );
    assert!(!text.contains("branch reflog"), "{text}");
}

/// A fast-forwarded branch has no pre-landing base to take a merge base with:
/// its landing commit is its own tip. Without a reflog the fork point is
/// therefore unknown, and the closure says so. It must not fall back to the
/// tip's parent, which would cover only the branch's last commit and report
/// "nothing changed" for a surface edit made by an earlier commit.
#[test]
fn closure_says_the_range_is_unknown_for_a_fast_forward_without_a_reflog() {
    let fx = landed_fixture_with(
        Landing::FastForward,
        &[("scripts/build.sh", "touch ignored\nexit 1\n")],
        false,
        |repo, branch| {
            git_ok(repo, &["checkout", "-q", branch]);
            write_file(repo, "src/work.py", "print('second commit')\n");
            git_ok(repo, &["add", "-A"]);
            git_ok(repo, &["commit", "-qm", "worker: second commit"]);
            git_ok(repo, &["checkout", "-q", "main"]);
            fs::remove_file(repo.join(".git/logs/refs/heads").join(branch)).unwrap();
        },
    );
    let out = done(&fx.repo, &fx.store, &fx.mol_id);
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("could not establish the branch's fork point or landing commit"),
        "an unknown range must be reported as unknown, not as unchanged:\n{text}"
    );
}

/// The range is fork point to landing commit, so a surface edit made by
/// another commit on the base inside that range is named too, although the
/// branch itself did not touch it.
#[test]
fn closure_names_a_surface_edit_made_by_another_commit_inside_the_range() {
    let fx = landed_fixture_with(
        Landing::NoFf,
        &[("src/work.py", "print('w')\n")],
        false,
        |repo, _branch| {
            write_file(repo, "scripts/build.sh", "touch ignored\nexit 1\n");
            git_ok(repo, &["add", "-A"]);
            git_ok(repo, &["commit", "-qm", "edit the gate script on main"]);
        },
    );
    let out = done(&fx.repo, &fx.store, &fx.mol_id);
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("scripts/build.sh") && text.contains("not reviewed"),
        "{text}"
    );
}
