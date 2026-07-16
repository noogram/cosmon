// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end tests for `cs migrate genre <NAME> --to <RESIDENCE>`.
//!
//! These exercise the bridge between the artifact-map (ADR-057) and
//! the residence primitive (ADR-055, e906/fa82). The canonical
//! operator scenario is the noesis case: `docs/surfaces/issues.md` and
//! `docs/surfaces/prs.md` are declared as genre `github-surface` with
//! `audience = "solo"`, yet they are still tracked in the index. One
//! command lands them in `.git/info/exclude` and drops them from the
//! index.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cs_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cs_in_repo(repo: &Path) -> Command {
    let mut cmd = cs_bin();
    cmd.current_dir(repo);
    cmd
}

fn run_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("git invocation failed in test setup");
    assert!(status.success(), "git {args:?} failed");
}

fn git_ls_files(repo: &Path, rel: &str) -> Vec<String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["ls-files", "--", rel])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn git_branches(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Seed a git repo with `.cosmon/artifact-map.toml` declaring a
/// `github-surface` genre over `docs/surfaces/**/*.md`, and two
/// surface files tracked in the index.
fn seed_github_surface_fixture(root: &Path) {
    run_git(root, &["init", "-q", "-b", "main"]);
    run_git(root, &["config", "user.email", "test@cosmon.local"]);
    run_git(root, &["config", "user.name", "Cosmon Test"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);

    fs::write(root.join("README.md"), "# fixture\n").unwrap();
    let cosmon = root.join(".cosmon");
    fs::create_dir_all(&cosmon).unwrap();
    fs::write(
        cosmon.join("artifact-map.toml"),
        r#"[github-surface]
location = ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"]
audience = "solo"

[code]
location = ["**/*"]
audience = "public"
"#,
    )
    .unwrap();

    let surfaces = root.join("docs/surfaces");
    fs::create_dir_all(&surfaces).unwrap();
    fs::write(surfaces.join("issues.md"), "# issues\n- one\n- two\n").unwrap();
    fs::write(surfaces.join("prs.md"), "# prs\n- a\n- b\n").unwrap();

    run_git(root, &["add", "README.md", ".cosmon", "docs"]);
    run_git(root, &["commit", "-q", "-m", "seed fixture"]);
}

/// Seed a fixture declaring the `chronicle` genre and one dated
/// chronicle file tracked on main.
fn seed_chronicle_fixture(root: &Path) {
    run_git(root, &["init", "-q", "-b", "main"]);
    run_git(root, &["config", "user.email", "test@cosmon.local"]);
    run_git(root, &["config", "user.name", "Cosmon Test"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);

    let cosmon = root.join(".cosmon");
    fs::create_dir_all(&cosmon).unwrap();
    fs::write(
        cosmon.join("artifact-map.toml"),
        r#"[chronicle]
location = ["docs/lore/**/*.md"]
audience = "author+agent"

[code]
location = ["**/*"]
audience = "public"
"#,
    )
    .unwrap();

    let lore = root.join("docs/lore");
    fs::create_dir_all(&lore).unwrap();
    fs::write(
        lore.join("CHRONICLES.md"),
        "# CHRONICLES\n\n2026-04-20: le pont entre le genre et la résidence.\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# fixture\n").unwrap();

    run_git(root, &["add", "README.md", ".cosmon", "docs"]);
    run_git(root, &["commit", "-q", "-m", "seed chronicle fixture"]);
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[test]
fn fixture_github_surface_to_solo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_github_surface_fixture(repo);

    // Precondition: surface files are tracked.
    let before = git_ls_files(repo, "docs/surfaces");
    assert_eq!(
        before.len(),
        2,
        "precondition: expected 2 surface files tracked, got {before:?}",
    );

    // Act: cs migrate genre github-surface --to solo --yes.
    let out = cs_in_repo(repo)
        .args([
            "migrate",
            "genre",
            "github-surface",
            "--to",
            "solo",
            "--yes",
        ])
        .output()
        .expect("cs migrate genre failed to spawn");
    assert!(
        out.status.success(),
        "migrate genre must succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Post: surface files untracked.
    let after = git_ls_files(repo, "docs/surfaces");
    assert!(
        after.is_empty(),
        "surface files must be untracked, got {after:?}",
    );

    // Post: info/exclude contains the genre location globs.
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert!(
        exclude.contains("docs/surfaces/**/*.md"),
        "info/exclude must carry the surface glob: {exclude:?}",
    );
    assert!(
        exclude.contains("STATUS.md"),
        "info/exclude must carry STATUS.md: {exclude:?}",
    );
    assert!(
        exclude.contains("ISSUES.md"),
        "info/exclude must carry ISSUES.md: {exclude:?}",
    );

    // Solo must NOT leak rules into shared .gitignore.
    let shared = repo.join(".gitignore");
    if shared.exists() {
        let contents = fs::read_to_string(&shared).unwrap();
        assert!(
            !contents.contains("docs/surfaces"),
            "solo must not touch shared .gitignore, got: {contents:?}",
        );
    }
}

#[test]
fn fixture_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_github_surface_fixture(repo);

    let first = cs_in_repo(repo)
        .args([
            "migrate",
            "genre",
            "github-surface",
            "--to",
            "solo",
            "--yes",
        ])
        .output()
        .expect("first run failed to spawn");
    assert!(
        first.status.success(),
        "first run must succeed: {}",
        String::from_utf8_lossy(&first.stderr),
    );

    let second = cs_in_repo(repo)
        .args([
            "migrate",
            "genre",
            "github-surface",
            "--to",
            "solo",
            "--yes",
        ])
        .output()
        .expect("second run failed to spawn");
    assert!(
        second.status.success(),
        "second run must succeed (idempotent): {}",
        String::from_utf8_lossy(&second.stderr),
    );

    // Check no duplicate exclude lines.
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    for pattern in ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"] {
        let count = exclude.lines().filter(|l| l.trim() == pattern).count();
        assert_eq!(
            count, 1,
            "pattern {pattern:?} must appear exactly once in info/exclude, got {count}: {exclude:?}",
        );
    }

    // Still no tracked surface files.
    assert!(git_ls_files(repo, "docs/surfaces").is_empty());
}

#[test]
fn fixture_dry_run() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_github_surface_fixture(repo);

    let out = cs_in_repo(repo)
        .args([
            "migrate",
            "genre",
            "github-surface",
            "--to",
            "solo",
            "--dry-run",
        ])
        .output()
        .expect("dry-run failed to spawn");
    assert!(
        out.status.success(),
        "dry-run must succeed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("DRY RUN"),
        "dry-run must announce itself: {stdout}",
    );

    // No mutation happened.
    let after = git_ls_files(repo, "docs/surfaces");
    assert_eq!(
        after.len(),
        2,
        "dry-run must not untrack anything, got {after:?}",
    );
    let exclude_path = repo.join(".git/info/exclude");
    if exclude_path.exists() {
        let exclude = fs::read_to_string(&exclude_path).unwrap();
        assert!(
            !exclude.contains("docs/surfaces"),
            "dry-run must not touch info/exclude, got: {exclude:?}",
        );
    }
}

#[test]
fn fixture_genre_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_github_surface_fixture(repo);

    let out = cs_in_repo(repo)
        .args([
            "migrate",
            "genre",
            "not-a-real-genre",
            "--to",
            "solo",
            "--yes",
        ])
        .output()
        .expect("unknown-genre run failed to spawn");
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        3,
        "unknown genre must exit 3 (got {code}): stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not-a-real-genre"),
        "error must name the unknown genre: {stderr}",
    );
}

#[test]
fn fixture_chronicle_to_team() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_chronicle_fixture(repo);

    let before = git_ls_files(repo, "docs/lore");
    assert_eq!(before.len(), 1, "precondition: one chronicle tracked");

    let out = cs_in_repo(repo)
        .args(["migrate", "genre", "chronicle", "--to", "team", "--yes"])
        .output()
        .expect("team migrate failed to spawn");
    assert!(
        out.status.success(),
        "team migrate must succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Post: orphan branch `cosmon/chronicle` exists.
    let branches = git_branches(repo);
    assert!(
        branches.iter().any(|b| b == "cosmon/chronicle"),
        "orphan branch cosmon/chronicle must exist, got {branches:?}",
    );

    // Post: chronicle file is untracked on main.
    let after_main = git_ls_files(repo, "docs/lore");
    assert!(
        after_main.is_empty(),
        "chronicle must be untracked on main, got {after_main:?}",
    );

    // Post: .gitignore on main contains the genre glob.
    let gitignore = fs::read_to_string(repo.join(".gitignore")).unwrap_or_default();
    assert!(
        gitignore.contains("docs/lore/**/*.md"),
        ".gitignore must carry the chronicle glob: {gitignore:?}",
    );

    // Post: the orphan branch contains the chronicle file.
    let out = Command::new("git")
        .current_dir(repo)
        .args(["ls-tree", "-r", "--name-only", "cosmon/chronicle"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        files.iter().any(|f| f == "docs/lore/CHRONICLES.md"),
        "orphan branch must carry the chronicle, got {files:?}",
    );
}

#[test]
fn fixture_encrypted_requires_age_or_recipient() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_github_surface_fixture(repo);

    // No --recipient passed — must fail at the prereq gate (exit 2).
    let out = cs_in_repo(repo)
        .args([
            "migrate",
            "genre",
            "github-surface",
            "--to",
            "encrypted",
            "--yes",
        ])
        .output()
        .expect("encrypted run failed to spawn");
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        2,
        "encrypted without --recipient must exit 2 (got {code}): stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn fixture_remote_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_github_surface_fixture(repo);

    let out = cs_in_repo(repo)
        .args([
            "migrate",
            "genre",
            "github-surface",
            "--to",
            "remote",
            "--yes",
        ])
        .output()
        .expect("remote run failed to spawn");
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        4,
        "remote must exit 4 (phase-2 gate): stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("phase-2") || stderr.contains("server setup"),
        "remote rejection must mention phase-2: {stderr}",
    );
}

/// `cs migrate genre --help` must list the residences.
#[test]
fn help_lists_residences() {
    let out = cs_bin()
        .args(["migrate", "genre", "--help"])
        .output()
        .expect("help failed to spawn");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for r in ["solo", "team", "encrypted", "remote"] {
        assert!(
            stdout.contains(r),
            "help must list residence {r:?}: {stdout}",
        );
    }
}

/// The genre module's `apply_genre_residence` is not exposed to tests,
/// but we can drive the whole pipeline from a path-less fixture
/// (operator runs the command from elsewhere via CWD).
#[test]
fn reachable_only_from_inside_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let non_repo = tmp.path();

    let out = cs_in_repo(non_repo)
        .args([
            "migrate",
            "genre",
            "github-surface",
            "--to",
            "solo",
            "--yes",
        ])
        .output()
        .expect("run failed to spawn");
    let code = out.status.code().unwrap_or(-1);
    // Not-a-git-repo → exit 2 (prereq) OR exit 3 (genre unknown because
    // we could not locate the map). Either way, non-zero with an
    // actionable stderr.
    assert_ne!(
        code,
        0,
        "must fail outside a git repo: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let _ = PathBuf::from(non_repo);
}
