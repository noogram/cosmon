// SPDX-License-Identifier: AGPL-3.0-only

//! Real-git assertions on the shipped `.cosmon/.gitignore` body (issue #60).
//!
//! Asserting the *text* of the ignore file is what let the defect ship: the
//! body announced that `state/archive/` was re-included by negation, the
//! text test agreed, and git ignored the subtree anyway — because git does
//! not descend into a directory excluded by `state/`, so `!state/archive/`
//! matched nothing.
//!
//! Every test here therefore runs a real `git` binary in a real temporary
//! repository and reads its verdict. Nothing re-implements ignore semantics;
//! that would be a second place for the same class of mistake to hide.

use std::path::Path;
use std::process::Command;

use cosmon_filestore::project_upgrade::{
    archive_subtree_ignored_rule, rewrite_cosmon_gitignore, COSMON_GITIGNORE_BLOCK_END,
    COSMON_GITIGNORE_BLOCK_START, COSMON_GITIGNORE_CONTENT,
    LEGACY_BROKEN_NEGATION_GITIGNORE_CONTENT,
};

/// A temporary git repository holding a `.cosmon/` with one archived
/// artifact, plus one ephemeral state file that must stay ignored.
struct Galaxy {
    dir: tempfile::TempDir,
}

const ARCHIVED: &str = ".cosmon/state/archive/2026/09/mol-x/result.md";
const EPHEMERAL: &str = ".cosmon/state/fleets/default/fleet.json";

impl Galaxy {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        run_git(root, &["init", "-q", "."]);
        for rel in [ARCHIVED, EPHEMERAL] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(&p, "hello\n").expect("write");
        }
        std::fs::write(root.join(".cosmon/registry.sqlite"), "").expect("write");
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write_ignore(&self, body: &str) {
        std::fs::write(self.root().join(".cosmon/.gitignore"), body).expect("write ignore");
    }

    /// Whether git actually ignores `rel`.
    ///
    /// The plain form is the one that answers the question: `-v` exits 0
    /// for a *negation* match too, so it reports a re-included path as if
    /// it were ignored.
    fn is_ignored(&self, rel: &str) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(["check-ignore", "--no-index", "--"])
            .arg(rel)
            .output()
            .expect("git check-ignore")
            .status
            .success()
    }

    /// The rule git says matched `rel`, in `<file>:<line>:<pattern>` form.
    fn matching_rule(&self, rel: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(["check-ignore", "-v", "--no-index", "--"])
            .arg(rel)
            .output()
            .expect("git check-ignore -v");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Paths a `git add -A -n` would stage, one per line.
    fn would_add(&self) -> Vec<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(["add", "-A", "-n"])
            .output()
            .expect("git add -n");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn run_git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

/// The transcript from issue #60, as a regression: the shipped-until-now
/// body ignores the archive despite its own comment saying otherwise.
#[test]
fn legacy_body_ignores_the_archive_it_claims_to_track() {
    let g = Galaxy::new();
    g.write_ignore(LEGACY_BROKEN_NEGATION_GITIGNORE_CONTENT);
    assert!(
        g.is_ignored(ARCHIVED),
        "legacy body must ignore the archive — that is the defect"
    );
    let verdict = g.matching_rule(ARCHIVED);
    assert!(
        verdict.contains(":state/\t"),
        "the excluding rule must be the blanket `state/`, got: {verdict}"
    );
    assert!(
        !g.would_add().iter().any(|l| l.contains("archive")),
        "no archive file may be stageable with the legacy body"
    );
}

/// The same transcript against the shipped body: green.
#[test]
fn current_body_tracks_the_archive_and_still_ignores_ephemeral_state() {
    let g = Galaxy::new();
    g.write_ignore(COSMON_GITIGNORE_CONTENT);

    assert!(
        !g.is_ignored(ARCHIVED),
        "the archive subtree must not be ignored, git says: {}",
        g.matching_rule(ARCHIVED)
    );
    assert!(
        g.is_ignored(EPHEMERAL),
        "ephemeral fleet state must stay ignored"
    );
    assert!(
        g.is_ignored(".cosmon/registry.sqlite"),
        "the registry must stay ignored"
    );

    let staged = g.would_add();
    assert!(
        staged.iter().any(|l| l.contains(ARCHIVED)),
        "archive artifact must be stageable, saw: {staged:?}"
    );
    assert!(
        !staged.iter().any(|l| l.contains("fleet.json")),
        "ephemeral state must not be stageable, saw: {staged:?}"
    );
}

/// What the file's comment claims is true of the rules beneath it: the
/// negation binds because the bulk exclusion is `state/*`, not `state/`.
#[test]
fn the_body_uses_the_form_its_comment_requires() {
    let lines: Vec<&str> = COSMON_GITIGNORE_CONTENT
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert!(lines.contains(&"state/*"), "must exclude with `state/*`");
    assert!(
        !lines.contains(&"state/"),
        "a blanket `state/` would stop git descending and void the negation"
    );
    assert!(lines.contains(&"!state/"));
    assert!(lines.contains(&"!state/archive/"));
    assert!(lines.contains(&"!state/archive/**"));
}

/// `archive_subtree_ignored_rule` is the probe `cs doctor` and
/// `cs init --upgrade` both read; it must agree with `git check-ignore`.
#[test]
fn the_probe_agrees_with_git() {
    let g = Galaxy::new();
    let cosmon_dir = g.root().join(".cosmon");

    g.write_ignore(LEGACY_BROKEN_NEGATION_GITIGNORE_CONTENT);
    assert!(
        archive_subtree_ignored_rule(g.root(), &cosmon_dir).is_some(),
        "probe must see the broken body as ignoring the archive"
    );

    g.write_ignore(COSMON_GITIGNORE_CONTENT);
    assert!(
        archive_subtree_ignored_rule(g.root(), &cosmon_dir).is_none(),
        "probe must see the current body as tracking the archive"
    );
}

/// The mangled-file case reported by a second user: a chain of rules that
/// ignore and re-include each other. The repair appends cosmon's block,
/// preserves every user line byte for byte, and real git then tracks the
/// archive.
#[test]
fn mangled_body_is_repaired_without_losing_a_user_line() {
    let mangled = "\
# hand-rolled by an agent, then patched three times
*.log
state/
!state/archive
state/archive/**
!state/**/synthesis.md
state/
my-own-secret-scratch/
";
    let g = Galaxy::new();
    g.write_ignore(mangled);
    let cosmon_dir = g.root().join(".cosmon");

    // RED: the mangled body ignores the archive.
    assert!(
        archive_subtree_ignored_rule(g.root(), &cosmon_dir).is_some(),
        "the mangled body must start out ignoring the archive"
    );

    // A customized body is NOT touched while nothing says it is broken.
    assert!(
        rewrite_cosmon_gitignore(mangled, false).is_none(),
        "exact-match discipline: a customized body is left alone by default"
    );

    let repaired = rewrite_cosmon_gitignore(mangled, true).expect("repair");
    assert!(
        repaired.starts_with(mangled),
        "every user line must be preserved byte for byte, ahead of the block"
    );
    assert!(repaired.contains(COSMON_GITIGNORE_BLOCK_START));
    assert!(repaired.contains(COSMON_GITIGNORE_BLOCK_END));

    // GREEN: real git now tracks the archive, and the user's own rules
    // (`*.log`, `my-own-secret-scratch/`) still bite.
    g.write_ignore(&repaired);
    assert_eq!(
        archive_subtree_ignored_rule(g.root(), &cosmon_dir),
        None,
        "the repaired body must track the archive"
    );
    std::fs::write(g.root().join(".cosmon/noise.log"), "x").expect("write");
    assert!(
        g.is_ignored(".cosmon/noise.log"),
        "the user's own `*.log` rule must survive the repair"
    );
}

/// Once the block is present, cosmon owns it and nothing else: a later
/// upgrade rewrites between the markers and leaves the rest alone.
#[test]
fn a_marked_body_is_rewritten_only_between_its_markers() {
    let stale = format!(
        "# my header\n\n{COSMON_GITIGNORE_BLOCK_START}\nstate/\n{COSMON_GITIGNORE_BLOCK_END}\n\n# my footer\nscratch/\n"
    );
    let updated = rewrite_cosmon_gitignore(&stale, false).expect("marked body is cosmon's to fix");
    assert!(updated.starts_with("# my header\n\n"));
    assert!(updated.ends_with("\n# my footer\nscratch/\n"));
    assert!(updated.contains("!state/archive/**"));
    assert!(
        rewrite_cosmon_gitignore(&updated, false).is_none(),
        "rewriting must be idempotent"
    );

    let g = Galaxy::new();
    g.write_ignore(&updated);
    assert_eq!(
        archive_subtree_ignored_rule(g.root(), &g.root().join(".cosmon")),
        None
    );
}

/// A recognised legacy body is still replaced wholesale — the migration
/// path that existed before the markers keeps working.
#[test]
fn legacy_body_is_replaced_wholesale() {
    assert_eq!(
        rewrite_cosmon_gitignore(LEGACY_BROKEN_NEGATION_GITIGNORE_CONTENT, false).as_deref(),
        Some(COSMON_GITIGNORE_CONTENT)
    );
    assert!(rewrite_cosmon_gitignore(COSMON_GITIGNORE_CONTENT, false).is_none());
}
