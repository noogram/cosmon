// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #69 — the molecule's `base_branch` has three writers, not one.
//!
//! Until this change only `cs tackle --base` could stamp a molecule's
//! integration base, so a germinated polymer could not be aimed at an
//! integration branch without tackling every node by hand. Now:
//!
//! * `cs nucleate --base <BRANCH>` (and a declaration's `base_branch`) stamps
//!   the base at birth, validated against the repository;
//! * `cs run --resident --base <BRANCH>` stamps it onto **pin-less** molecules
//!   as the loop dispatches them — the per-molecule base wins, exactly as the
//!   per-molecule adapter pin wins over `cs run --adapter`.
//!
//! Every test runs the **real `cs` binary** against a throwaway repository.
//! Dispatches are deliberately doomed (`--adapter anthropic` with no API key):
//! `cs tackle` resolves, validates and persists the base before it reaches the
//! spawn step, which gives these tests the persistence half of a dispatch
//! without a live model or a tmux server.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `cs` pinned to the repo's isolated state, with every ambient variable that
/// could name a base or poison a dispatch stripped.
fn cs(repo: &Path) -> Command {
    let state = repo.join(".cosmon/state");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    for var in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_BASE_BRANCH",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_MODEL",
        "COSMON_DEFAULT_MODEL",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_EGRESS_POLICY",
        "CB_DEPTH",
    ] {
        cmd.env_remove(var);
    }
    cmd.env("COSMON_STATE_DIR", &state)
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .current_dir(&state);
    cmd
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository on `main` with branches `feat/x` and `feat/y`, a `.cosmon`
/// project (state gitignored) and the `task-work` formula. No `origin`.
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
        "[project]\nproject_id = \"base-writers-a69a\"\n",
    )
    .unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    fs::write(repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);
    git_ok(repo, &["branch", "feat/x"]);
    git_ok(repo, &["branch", "feat/y"]);
}

fn molecules_dir(repo: &Path) -> PathBuf {
    repo.join(".cosmon/state/fleets/default/molecules")
}

fn molecule_ids(repo: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(molecules_dir(repo)) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join("state.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// The molecule's persisted `base_branch`, read from `state.json`.
fn persisted_base(repo: &Path, mol_id: &str) -> Option<String> {
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(molecules_dir(repo).join(mol_id).join("state.json")).expect("read state"),
    )
    .expect("parse state");
    state["base_branch"].as_str().map(str::to_owned)
}

/// `cs nucleate task-work`, with extra arguments, returning the new id.
fn nucleate(repo: &Path, extra: &[&str]) -> String {
    let out = cs(repo)
        .args(["--json", "nucleate", "task-work", "--var", "topic=issue-69"])
        .args(extra)
        .output()
        .expect("cs nucleate");
    assert!(out.status.success(), "nucleate failed: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["id"].as_str().expect("nucleate id").to_owned()
}

/// A `cs tackle` that persists its base and then fails at the spawn step.
fn doomed_tackle(repo: &Path, mol_id: &str, extra: &[&str]) {
    let out = cs(repo)
        .args(["tackle", mol_id, "--adapter", "anthropic", "--force"])
        .args(extra)
        .output()
        .expect("cs tackle");
    assert!(
        !out.status.success(),
        "precondition: the doomed dispatch must fail at spawn"
    );
    assert!(
        stderr(&out).contains("ANTHROPIC_API_KEY"),
        "precondition: the dispatch must fail at the credential step, after the base \
         is handled — not earlier: {}",
        stderr(&out)
    );
}

/// A bounded resident run; it ends at its deadline because every dispatch
/// is doomed, so only its effect on state is asserted.
fn resident_run(repo: &Path, base: &str) {
    let out = cs(repo)
        .args([
            "run",
            "--resident",
            "--base",
            base,
            "--adapter",
            "anthropic",
            "--timeout",
            "4",
        ])
        .output()
        .expect("cs run");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("resident runtime"),
        "the resident run must start, not refuse its base: {}",
        stderr(&out)
    );
}

/// The merge target `cs done --dry-run` reports for a molecule.
fn dry_run_merge_target(repo: &Path, mol_id: &str) -> String {
    let collapse = cs(repo)
        .args(["--json", "collapse", mol_id, "--reason", "integration test"])
        .output()
        .expect("cs collapse");
    assert!(collapse.status.success(), "collapse: {}", stderr(&collapse));
    let out = cs(repo)
        .args(["--json", "done", mol_id, "--dry-run"])
        .output()
        .expect("cs done --dry-run");
    assert!(out.status.success(), "done --dry-run: {}", stderr(&out));
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "dry-run JSON: {e}: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    plan["merge_target"]
        .as_str()
        .unwrap_or_else(|| panic!("no merge_target in {plan}"))
        .to_owned()
}

/// Falsifier 1 — the polymer case that motivated the issue. Three base-less
/// molecules hydrated from a directory of declarations; `cs run --resident
/// --base feat/x` aims every one of them, and `cs done --dry-run` then reports
/// `feat/x` as the merge target although the checkout is on `main`.
#[test]
fn run_base_aims_a_baseless_polymer() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let decls = repo.join("decls");
    fs::create_dir_all(&decls).unwrap();
    for n in 1..=3 {
        fs::write(
            decls.join(format!("node-{n}.toml")),
            format!(
                "id_prefix = \"task\"\nformula = \"task-work\"\n\n[variables]\ntopic = \"node {n}\"\n"
            ),
        )
        .unwrap();
    }
    let out = cs(repo)
        .args(["--json", "nucleate", "--from"])
        .arg(&decls)
        .output()
        .expect("cs nucleate --from");
    assert!(out.status.success(), "nucleate --from: {}", stderr(&out));

    let ids = molecule_ids(repo);
    assert_eq!(ids.len(), 3);
    for id in &ids {
        assert_eq!(persisted_base(repo, id), None, "born base-less");
    }

    resident_run(repo, "feat/x");

    for id in &ids {
        assert_eq!(
            persisted_base(repo, id).as_deref(),
            Some("feat/x"),
            "{id} must be aimed at the run-wide base"
        );
    }
    assert_eq!(dry_run_merge_target(repo, &ids[0]), "feat/x");
}

/// Falsifier 2 — `cs nucleate --base feat/x` persists the base; a bare
/// `cs tackle` keeps it, and the harvest resolves to it rather than to the
/// checked-out `main`.
#[test]
fn nucleate_base_persists_and_a_bare_tackle_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let id = nucleate(repo, &["--base", "feat/x"]);
    assert_eq!(persisted_base(repo, &id).as_deref(), Some("feat/x"));

    doomed_tackle(repo, &id, &[]);
    assert_eq!(persisted_base(repo, &id).as_deref(), Some("feat/x"));
    assert_eq!(dry_run_merge_target(repo, &id), "feat/x");
}

/// Falsifier 3 — `cs tackle --base feat/y` still overrides a birth base, and
/// the override is persisted.
#[test]
fn tackle_base_overrides_the_birth_base() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let id = nucleate(repo, &["--base", "feat/x"]);
    doomed_tackle(repo, &id, &["--base", "feat/y"]);
    assert_eq!(persisted_base(repo, &id).as_deref(), Some("feat/y"));
}

/// Falsifier 4 — `cs run --base feat/z` does not overwrite a molecule already
/// pinned to `feat/x`, while a pin-less sibling in the same run does take it.
#[test]
fn run_base_does_not_overwrite_a_pinned_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    git_ok(repo, &["branch", "feat/z"]);

    let pinned = nucleate(repo, &["--base", "feat/x"]);
    let pinless = nucleate(repo, &[]);

    resident_run(repo, "feat/z");

    assert_eq!(persisted_base(repo, &pinned).as_deref(), Some("feat/x"));
    assert_eq!(persisted_base(repo, &pinless).as_deref(), Some("feat/z"));
}

/// Falsifier 5 — a base naming no branch is refused at nucleation, naming the
/// branch, and no molecule is created — on the flag and in a declaration
/// directory alike (where a valid sibling declaration must not be born either).
#[test]
fn a_dangling_base_is_refused_at_birth() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let out = cs(repo)
        .args([
            "nucleate",
            "task-work",
            "--var",
            "topic=x",
            "--base",
            "does-not-exist",
        ])
        .output()
        .expect("cs nucleate");
    assert!(!out.status.success(), "a dangling base must be refused");
    assert!(
        stderr(&out).contains("does-not-exist"),
        "the refusal must name the branch: {}",
        stderr(&out)
    );
    assert!(molecule_ids(repo).is_empty(), "no molecule may be created");

    let decls = repo.join("decls");
    fs::create_dir_all(&decls).unwrap();
    fs::write(
        decls.join("a-good.toml"),
        "id_prefix = \"task\"\nformula = \"task-work\"\nbase_branch = \"feat/x\"\n\n[variables]\ntopic = \"t\"\n",
    )
    .unwrap();
    fs::write(
        decls.join("b-bad.toml"),
        "id_prefix = \"task\"\nformula = \"task-work\"\nbase_branch = \"does-not-exist\"\n\n[variables]\ntopic = \"t\"\n",
    )
    .unwrap();
    let out = cs(repo)
        .args(["nucleate", "--from"])
        .arg(&decls)
        .output()
        .expect("cs nucleate --from");
    assert!(!out.status.success(), "a dangling declared base is refused");
    assert!(stderr(&out).contains("does-not-exist"), "{}", stderr(&out));
    assert!(
        molecule_ids(repo).is_empty(),
        "no declaration in the directory may be born behind the refusal"
    );

    fs::remove_file(decls.join("b-bad.toml")).unwrap();
    let out = cs(repo)
        .args(["nucleate", "--from"])
        .arg(&decls)
        .output()
        .expect("cs nucleate --from");
    assert!(out.status.success(), "{}", stderr(&out));
    let ids = molecule_ids(repo);
    assert_eq!(ids.len(), 1);
    assert_eq!(persisted_base(repo, &ids[0]).as_deref(), Some("feat/x"));
}

/// Falsifier 6 — without `--base` no default is minted: the persisted base
/// stays empty and the harvest resolves the ambient chain (`main`).
#[test]
fn nucleate_without_base_mints_none() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let id = nucleate(repo, &[]);
    assert_eq!(persisted_base(repo, &id), None);
    assert_eq!(dry_run_merge_target(repo, &id), "main");
}

/// `cs run --base` is a resident-mode directive; outside `--resident` it would
/// be silently ignored, so clap refuses it instead.
#[test]
fn run_base_requires_resident_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let out = cs(repo)
        .args(["run", "some-root", "--base", "feat/x"])
        .output()
        .expect("cs run");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--resident"), "{}", stderr(&out));
}
