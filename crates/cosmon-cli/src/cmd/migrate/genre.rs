// SPDX-License-Identifier: AGPL-3.0-only

//! `cs migrate genre <NAME> --to <RESIDENCE>` — apply a residence to
//! every tracked file classified under a given genre.
//!
//! This verb bridges two earlier primitives:
//!
//! * **artifact-map.toml** (ADR-057) declares *what* each tracked path
//!   is (a "genre") and who it is addressed to.
//! * **`cs migrate to <residence>`** (ADR-055, tasks e906 / fa82) moves
//!   the whole galaxy's memory to a residence atomically, with the
//!   data + git index guarded by a BLAKE3 manifest.
//!
//! `cs migrate genre` is the *scoped* version of the latter: it targets
//! only the files classified under one genre, not the whole state tree.
//! The composition pattern is *classify → apply → persist*:
//!
//! 1. **Classify** — load `.cosmon/artifact-map.toml`, find the genre,
//!    walk `git ls-files` and retain paths that match the genre's globs.
//! 2. **Apply** — run the residence's git-side transition over the
//!    matched paths only, reusing the helpers exposed by the parent
//!    module (`run_git`, `append_line_if_missing`, `find_git_root`, …).
//! 3. **Persist** — update the genre's `audience` in
//!    `.cosmon/artifact-map.toml` so future classifications converge on
//!    the newly chosen residence.
//!
//! # Operator use case (noesis, 2026-04-20)
//!
//! The `github-surface` genre is declared `audience = "solo"`, but
//! `docs/surfaces/issues.md` and `docs/surfaces/prs.md` are still
//! tracked and pollute `git log` with `chore(surfaces)` commits. The
//! operator runs:
//!
//! ```text
//! cs migrate genre github-surface --to solo
//! ```
//!
//! which untracks the matched files (`git rm -r --cached`), appends the
//! genre's location globs to `.git/info/exclude`, and commits
//! `chore(cosmon): migrate genre github-surface to solo residence`. The
//! genre declaration in `artifact-map.toml` already reads
//! `audience = "solo"`, so the persist step is a no-op — a silent tell
//! that the map is now in sync with reality.
//!
//! # Exit codes (scripting contract)
//!
//! * `0` — success.
//! * `2` — prereq failure (not a git repo, missing tools, dirty index
//!   when `--yes` was passed, …).
//! * `3` — genre unknown (not declared in `artifact-map.toml`).
//! * `4` — residence transition failed (git error, orphan branch seed
//!   failed, …).
//! * `5` — user aborted at the confirmation prompt.
//!
//! # Scope in v0
//!
//! * **solo** — fully implemented (the noesis case).
//! * **team** — orphan branch `cosmon/<genre>` is seeded from the
//!   matched paths; the current branch untracks them and appends them
//!   to `.gitignore`. The pattern mirrors the `cosmon/state` orphan
//!   branch.
//! * **encrypted** — refused unless the `age` binary is on PATH and a
//!   `--recipient` is provided; age wrapping itself is deferred.
//! * **remote** — refused with a message pointing at the cosmon-saas
//!   phase-2 server setup. No silent failure.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command as ShellCommand;

use cosmon_core::artifact_map::{ArtifactMap, GenreSpec};

use super::{append_line_if_missing, find_git_root, ignore_file_for, run_git, Residence};
use crate::cmd::inspect::load_map;
use crate::cmd::Context;

// ─── Exit codes ─────────────────────────────────────────────────────────

/// Prereq failure (not a git repo, tool missing, dirty index, …).
const EXIT_PREREQ_FAIL: i32 = 2;
/// Requested genre is not declared in `artifact-map.toml`.
const EXIT_GENRE_UNKNOWN: i32 = 3;
/// Residence transition failed after prereqs passed (git error, …).
const EXIT_RESIDENCE_FAIL: i32 = 4;
/// Operator answered "no" at the confirmation prompt.
const EXIT_USER_ABORT: i32 = 5;

// ─── Arguments ──────────────────────────────────────────────────────────

/// Arguments for `cs migrate genre <NAME> --to <RESIDENCE>`.
#[derive(clap::Args, Debug)]
pub struct GenreArgs {
    /// Genre name (must match a `[<name>]` table in
    /// `.cosmon/artifact-map.toml`).
    pub name: String,

    /// Target residence.
    #[arg(long, value_name = "RESIDENCE")]
    pub to: Residence,

    /// After the residence transition, invoke
    /// `cs git scrub-history --path <paths>` (when available) to
    /// rewrite prior commits that touched the matched paths.
    ///
    /// Soft dependency on `cs git scrub-history`. When the subcommand is
    /// not yet implemented, this flag degrades to a one-line warning
    /// and the migration still succeeds on the current tree.
    #[arg(long = "scrub-history")]
    pub scrub_history: bool,

    /// `age` recipient to wrap the narration for, when
    /// `--to encrypted` is set. Required for the encrypted path.
    #[arg(long, value_name = "AGE_RECIPIENT")]
    pub recipient: Option<String>,

    /// Preview the plan without writing to disk or touching git.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the interactive confirmation prompt.
    #[arg(long, short)]
    pub yes: bool,
}

// ─── Plan + report ──────────────────────────────────────────────────────

/// What `cs migrate genre` intends to do — surfaced in dry-run + the
/// confirmation prompt + the final report. Deliberately stringly-typed:
/// the command is a thin shell over git mutations that are already
/// described verbatim to the operator.
#[derive(Debug, Clone)]
pub struct MigrationPlan {
    /// Genre name being migrated.
    pub genre: String,
    /// Target residence (lowercase string — matches the TOML).
    pub residence: String,
    /// Tracked paths matched by the genre (relative to repo root).
    pub matched_paths: Vec<String>,
    /// Glob patterns the genre declares (mirrored into the ignore file).
    pub locations: Vec<String>,
    /// Ignore file the plan will touch.
    pub ignore_file_rel: String,
    /// Name of the orphan branch, when the residence creates one.
    pub orphan_branch: Option<String>,
    /// Noop flag set when matched paths are empty and the artifact-map
    /// already declares the target residence.
    pub noop: bool,
}

/// Report returned by [`apply_genre_residence`]. Consumed by both the
/// human pretty-printer and the `--json` path.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GenreMigrationReport {
    /// Target residence (`"solo"`, `"team"`, `"encrypted"`, `"remote"`).
    pub residence: String,
    /// Genre name.
    pub genre: String,
    /// Number of paths transitioned from *tracked* to *untracked*.
    pub untracked_count: usize,
    /// Number of location globs appended to the ignore file (0, 1, …).
    pub locations_added: usize,
    /// Ignore file path (relative to repo root).
    pub ignore_file: String,
    /// Orphan branch seeded (team/encrypted) or none.
    pub orphan_branch: Option<String>,
    /// SHA of the residence commit (empty when `--no-commit`-equivalent
    /// path is taken, i.e. nothing to commit).
    pub commit_sha: Option<String>,
    /// Whether `artifact-map.toml` was rewritten to persist the new
    /// audience.
    pub map_persisted: bool,
    /// Whether the migration was a no-op (idempotent re-run).
    pub noop: bool,
}

// ─── Public API — dispatch + the pure planner ──────────────────────────

/// Execute `cs migrate genre`.
///
/// # Errors
///
/// Returns an `anyhow::Error` for unexpected I/O or git failures; known
/// error classes exit with the dedicated codes documented on the
/// module.
pub fn run(ctx: &Context, args: &GenreArgs) -> anyhow::Result<()> {
    // Remote residence — not implemented in v0.
    if matches!(args.to, Residence::Remote) {
        emit_error(
            ctx,
            "remote residence requires phase-2 server setup (see cosmon-saas-prototype)",
        );
        std::process::exit(EXIT_RESIDENCE_FAIL);
    }

    // Encrypted residence — require --recipient AND `age` on PATH.
    if matches!(args.to, Residence::Encrypted) {
        if args.recipient.is_none() {
            emit_error(
                ctx,
                "encrypted residence requires --recipient <age1...> (see ADR-056)",
            );
            std::process::exit(EXIT_PREREQ_FAIL);
        }
        if !age_available() {
            emit_error(
                ctx,
                "encrypted residence requires the `age` binary on PATH (brew install age)",
            );
            std::process::exit(EXIT_PREREQ_FAIL);
        }
    }

    let map = load_map()?;
    let Some(genre_spec) = map.genres.iter().find(|g| g.name == args.name).cloned() else {
        emit_error(
            ctx,
            &format!(
                "genre '{}' is not declared in .cosmon/artifact-map.toml",
                args.name
            ),
        );
        std::process::exit(EXIT_GENRE_UNKNOWN);
    };

    let cwd = std::env::current_dir()?;
    let Some(repo_root) = find_git_root(&cwd) else {
        emit_error(ctx, "not inside a git repository");
        std::process::exit(EXIT_PREREQ_FAIL);
    };

    // Walk `git ls-files` and classify each one. Matching files go into
    // the plan; the rest are untouched.
    let all_tracked = git_ls_all_files(&repo_root)?;
    let matched_paths: Vec<String> = all_tracked
        .iter()
        .filter(|p| {
            map.classify(Path::new(p))
                .as_ref()
                .is_some_and(|c| c.genre == args.name)
        })
        .cloned()
        .collect();

    let plan = build_plan(&genre_spec, &repo_root, &matched_paths, args.to, &map);

    // Dry run: print the plan and exit successfully. Nothing touches
    // disk. No confirmation prompt is raised.
    if args.dry_run {
        render_plan(ctx, &plan, true);
        return Ok(());
    }

    // Idempotent no-op: nothing to do.
    if plan.noop {
        let report = GenreMigrationReport {
            residence: plan.residence.clone(),
            genre: plan.genre.clone(),
            untracked_count: 0,
            locations_added: 0,
            ignore_file: plan.ignore_file_rel.clone(),
            orphan_branch: None,
            commit_sha: None,
            map_persisted: false,
            noop: true,
        };
        render_report(ctx, &plan, &report);
        return Ok(());
    }

    // Confirmation prompt — skipped with --yes.
    if !args.yes && !confirm_interactively(&plan) {
        emit_error(ctx, "aborted by user");
        std::process::exit(EXIT_USER_ABORT);
    }

    // Apply the plan. Any failure here maps to exit 4.
    let outcome = apply_genre_residence(&repo_root, &genre_spec, &plan);
    let report = match outcome {
        Ok(r) => r,
        Err(e) => {
            emit_error(ctx, &format!("residence transition failed: {e}"));
            std::process::exit(EXIT_RESIDENCE_FAIL);
        }
    };

    // `--scrub-history` soft dep: print the warning exactly once, at
    // the end. Never fails the command.
    if args.scrub_history {
        warn_scrub_history(ctx);
    }

    render_report(ctx, &plan, &report);
    Ok(())
}

/// Build a [`MigrationPlan`] from the classified matches.
///
/// Pure: no I/O, no mutation. The plan carries enough to drive both
/// the dry-run printout and the live apply phase. `noop` is set when
/// there is nothing to untrack *and* the artifact-map already declares
/// the target residence — the definition of an idempotent re-run.
fn build_plan(
    spec: &GenreSpec,
    repo_root: &Path,
    matched: &[String],
    target: Residence,
    map: &ArtifactMap,
) -> MigrationPlan {
    let (_, ignore_rel) = ignore_file_for(target, repo_root);
    let orphan_branch = match target {
        Residence::Team | Residence::Encrypted => Some(format!("cosmon/{}", spec.name)),
        Residence::Solo | Residence::Remote => None,
    };

    let audience_already_matches = map
        .genres
        .iter()
        .find(|g| g.name == spec.name)
        .is_some_and(|g| audience_matches_residence(g, target));

    let noop = matched.is_empty() && audience_already_matches;

    MigrationPlan {
        genre: spec.name.clone(),
        residence: residence_str(target).to_owned(),
        matched_paths: matched.to_vec(),
        locations: spec.locations.clone(),
        ignore_file_rel: ignore_rel.to_owned(),
        orphan_branch,
        noop,
    }
}

/// Apply the planned residence transition.
///
/// # Errors
///
/// Surfaces git errors and I/O failures verbatim. The caller converts
/// them into an exit code.
pub fn apply_genre_residence(
    repo_root: &Path,
    spec: &GenreSpec,
    plan: &MigrationPlan,
) -> anyhow::Result<GenreMigrationReport> {
    // Clean-index precondition — mirror the `cs migrate to` contract
    // so the commit we produce stays focused on the genre migration.
    let (_stdout, _stderr, clean) = run_git(repo_root, &["diff", "--cached", "--quiet"])?;
    if !clean {
        anyhow::bail!(
            "refusing to commit genre migration: index is not clean. \
             Commit or stash staged changes first, then retry."
        );
    }

    let target = residence_from_str(&plan.residence)?;
    let gitignore_is_shared = matches!(
        target,
        Residence::Team | Residence::Encrypted | Residence::Remote
    );

    // --- 1. Seed orphan branch for team/encrypted residences. --------
    let mut orphan_branch_created = false;
    if let Some(branch) = &plan.orphan_branch {
        orphan_branch_created = seed_orphan_branch(repo_root, branch, &plan.matched_paths)?;
    }

    // --- 2. Untrack the matched paths on the current branch. ---------
    let untracked_count = if plan.matched_paths.is_empty() {
        0
    } else {
        let mut argv: Vec<&str> = vec!["rm", "-r", "--cached", "--ignore-unmatch", "--quiet"];
        for p in &plan.matched_paths {
            argv.push(p);
        }
        let (_out, err, ok) = run_git(repo_root, &argv)?;
        if !ok {
            anyhow::bail!("git rm --cached failed: {err}");
        }
        plan.matched_paths.len()
    };

    // --- 3. Append the genre's location globs to the ignore file. ---
    let (ignore_path, ignore_rel) = ignore_file_for(target, repo_root);
    let mut locations_added = 0usize;
    for pattern in &plan.locations {
        if append_line_if_missing(&ignore_path, pattern)? {
            locations_added += 1;
        }
    }

    // For shared ignore files (team-class residences), stage the edit
    // so it rolls into the migration commit.
    if locations_added > 0 && gitignore_is_shared {
        let (_o, err, ok) = run_git(repo_root, &["add", "--", ignore_rel])?;
        if !ok {
            anyhow::bail!("git add {ignore_rel} failed: {err}");
        }
    }

    // --- 4. Commit, scoped to the migration subject. -----------------
    let mut commit_sha: Option<String> = None;
    let has_untracking = untracked_count > 0;
    let has_shared_ignore_change = gitignore_is_shared && locations_added > 0;
    if has_untracking || has_shared_ignore_change {
        let subject = format!(
            "chore(cosmon): migrate genre {} to {} residence",
            spec.name, plan.residence
        );
        let (_o, err, ok) = run_git(repo_root, &["commit", "--quiet", "-m", &subject])?;
        if ok {
            commit_sha = git_head_sha(repo_root);
        } else if !err.contains("nothing to commit") && !err.contains("nothing added") {
            anyhow::bail!("git commit failed: {err}");
        }
    }

    // --- 5. Persist audience in artifact-map.toml. ------------------
    let map_persisted = persist_audience_for_genre(repo_root, &spec.name, target)?;
    if map_persisted && gitignore_is_shared {
        // Stage + commit the map rewrite as its own tiny follow-up
        // so the artifact-map is always in lockstep with reality.
        let (_o, err, ok) = run_git(repo_root, &["add", "--", ".cosmon/artifact-map.toml"])?;
        if !ok {
            anyhow::bail!("git add .cosmon/artifact-map.toml failed: {err}");
        }
        let (_o, err, ok) = run_git(
            repo_root,
            &[
                "commit",
                "--quiet",
                "-m",
                &format!(
                    "chore(cosmon): artifact-map — genre {} audience now {}",
                    spec.name,
                    residence_to_audience_literal(target),
                ),
            ],
        )?;
        if !ok && !err.contains("nothing to commit") && !err.contains("nothing added") {
            anyhow::bail!("git commit (artifact-map) failed: {err}");
        }
    }

    Ok(GenreMigrationReport {
        residence: plan.residence.clone(),
        genre: plan.genre.clone(),
        untracked_count,
        locations_added,
        ignore_file: plan.ignore_file_rel.clone(),
        orphan_branch: if orphan_branch_created {
            plan.orphan_branch.clone()
        } else {
            None
        },
        commit_sha,
        map_persisted,
        noop: false,
    })
}

// ─── Helpers ────────────────────────────────────────────────────────────

fn residence_str(r: Residence) -> &'static str {
    match r {
        Residence::Solo => "solo",
        Residence::Team => "team",
        Residence::Encrypted => "encrypted",
        Residence::Remote => "remote",
    }
}

fn residence_from_str(s: &str) -> anyhow::Result<Residence> {
    match s {
        "solo" => Ok(Residence::Solo),
        "team" => Ok(Residence::Team),
        "encrypted" => Ok(Residence::Encrypted),
        "remote" => Ok(Residence::Remote),
        other => anyhow::bail!("unknown residence literal: {other}"),
    }
}

/// Map a residence to the canonical audience we write back into the
/// artifact map. Solo → `"solo"`. Everything else → `"author+agent"`
/// (the narration default — operators with richer intent (`team`,
/// `public`, `partner:<name>`) can re-edit by hand). This is a
/// one-direction projection: we never infer `public` from a residence
/// choice.
fn residence_to_audience_literal(r: Residence) -> &'static str {
    match r {
        Residence::Solo => "solo",
        Residence::Team | Residence::Encrypted | Residence::Remote => "author+agent",
    }
}

/// True when the genre's current audience already projects onto the
/// target residence (audience → residence is the derivation in
/// `ArtifactMap`). Used to decide if [`build_plan`] marks the run noop.
fn audience_matches_residence(spec: &GenreSpec, target: Residence) -> bool {
    use cosmon_core::artifact_map::AudienceSpec;
    let audience_is_solo = matches!(spec.audience, AudienceSpec::Solo);
    match target {
        Residence::Solo => audience_is_solo,
        Residence::Team | Residence::Encrypted | Residence::Remote => !audience_is_solo,
    }
}

/// Return the list of every path in the index (repo-relative).
fn git_ls_all_files(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    let (stdout, err, ok) = run_git(repo_root, &["ls-files"])?;
    if !ok {
        anyhow::bail!("git ls-files failed: {err}");
    }
    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

fn git_head_sha(repo_root: &Path) -> Option<String> {
    let out = ShellCommand::new("git")
        .current_dir(repo_root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// Create (or update) an orphan branch holding exactly the genre's
/// matched paths. Mirrors the `cosmon/state` seeding pattern, but
/// scoped to a single genre. Returns `true`
/// when a new commit was created, `false` when the branch was already
/// up to date (idempotent re-run).
fn seed_orphan_branch(repo_root: &Path, branch: &str, paths: &[String]) -> anyhow::Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }

    // Snapshot the current branch so we can return the operator there.
    let (cur_branch, _err, ok) = run_git(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if !ok {
        anyhow::bail!("git rev-parse --abbrev-ref HEAD failed");
    }
    let cur_branch = cur_branch.trim().to_owned();

    // Snapshot each matched file's current content from the working
    // tree. We keep the bytes in memory and write them back after
    // switching to the orphan branch. Working-tree reads (rather than
    // `git show HEAD:<path>`) let us seed the orphan from the live
    // content even when the operator made staged edits on top.
    let snapshots: Vec<(String, Vec<u8>)> = paths
        .iter()
        .filter_map(|p| {
            let abs = repo_root.join(p);
            fs::read(&abs).ok().map(|bytes| (p.clone(), bytes))
        })
        .collect();

    // Does the branch already exist? If so, switch; else create orphan.
    let (_o, _e, branch_exists) = run_git(
        repo_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    if branch_exists {
        let (_o, err, ok) = run_git(repo_root, &["switch", "--quiet", branch])?;
        if !ok {
            anyhow::bail!("git switch {branch} failed: {err}");
        }
    } else {
        let (_o, err, ok) = run_git(repo_root, &["switch", "--quiet", "--orphan", branch])?;
        if !ok {
            anyhow::bail!("git switch --orphan {branch} failed: {err}");
        }
        // An orphan starts with the current index — clear it so the
        // orphan only carries the genre's files.
        let (_o, err, ok) = run_git(
            repo_root,
            &["rm", "-rf", "--quiet", "--ignore-unmatch", "."],
        )?;
        if !ok {
            anyhow::bail!("git rm -rf . on orphan failed: {err}");
        }
    }

    // Rewrite the working tree to the snapshots + stage them.
    for (rel, bytes) in &snapshots {
        let abs = repo_root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&abs, bytes)?;
    }
    let mut argv: Vec<&str> = vec!["add", "--"];
    for (rel, _) in &snapshots {
        argv.push(rel.as_str());
    }
    let (_o, err, ok) = run_git(repo_root, &argv)?;
    if !ok {
        anyhow::bail!("git add (orphan) failed: {err}");
    }

    // Commit — nothing-to-commit is a fine idempotent outcome.
    let subject = format!("chore(cosmon): seed orphan branch {branch}");
    let (_o, err, ok) = run_git(repo_root, &["commit", "--quiet", "-m", &subject])?;
    let mut created = ok;
    if !ok {
        if !err.contains("nothing to commit") && !err.contains("nothing added") {
            anyhow::bail!("git commit (orphan) failed: {err}");
        }
        created = false;
    }

    // Return to the original branch. This may leave the working tree
    // with the genre's files present — but that is what we want, the
    // caller will `git rm -r --cached` them next.
    let (_o, err, ok) = run_git(repo_root, &["switch", "--quiet", &cur_branch])?;
    if !ok {
        anyhow::bail!("git switch {cur_branch} failed: {err}");
    }

    Ok(created)
}

/// Rewrite `.cosmon/artifact-map.toml` so the genre's `audience`
/// matches the target residence. Returns `true` when a write
/// happened. Best-effort: missing map is ignored (the operator has
/// no map to update, which is its own declaration).
fn persist_audience_for_genre(
    repo_root: &Path,
    genre: &str,
    target: Residence,
) -> anyhow::Result<bool> {
    let map_path = repo_root.join(".cosmon/artifact-map.toml");
    if !map_path.exists() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&map_path)?;
    let mut doc: toml::Value = toml::from_str(&raw)?;
    let target_audience = residence_to_audience_literal(target);

    let toml::Value::Table(ref mut root) = doc else {
        return Ok(false);
    };
    let Some(entry) = root.get_mut(genre) else {
        // Genre not in TOML — nothing to persist (the load_map call
        // that got us here must have merged from a different source).
        return Ok(false);
    };
    let toml::Value::Table(ref mut table) = entry else {
        return Ok(false);
    };
    let current = table
        .get("audience")
        .and_then(toml::Value::as_str)
        .unwrap_or("")
        .to_owned();
    if current == target_audience {
        return Ok(false);
    }
    table.insert(
        "audience".to_owned(),
        toml::Value::String(target_audience.to_owned()),
    );

    let new_text = toml::to_string_pretty(&doc)?;
    fs::write(&map_path, new_text)?;
    Ok(true)
}

/// Is the `age` binary on `$PATH`?
fn age_available() -> bool {
    ShellCommand::new("age")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Read a y/n from stdin. Defaults to `false` on any read error — we
/// prefer refusing over guessing.
fn confirm_interactively(plan: &MigrationPlan) -> bool {
    if !atty_stdin() {
        // Non-interactive invocation without --yes: refuse rather than
        // silently proceeding. The operator asked us to ask.
        return false;
    }
    println!();
    print_plan_human(plan, /*dry_run=*/ false);
    print!("\nApply this plan? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let answer = line.trim().to_ascii_lowercase();
    matches!(answer.as_str(), "y" | "yes")
}

/// `isatty(stdin)` via `std::io::IsTerminal`.
fn atty_stdin() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn emit_error(ctx: &Context, msg: &str) {
    if ctx.json {
        let payload = serde_json::json!({
            "command": "migrate genre",
            "status": "error",
            "error": msg,
        });
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| msg.to_owned())
        );
    } else {
        eprintln!("error: {msg}");
    }
}

fn warn_scrub_history(ctx: &Context) {
    let msg = "--scrub-history: `cs git scrub-history` is not yet available \
         (task-20260420-3a02 in flight). Rewrite manually with `git filter-repo` \
         if prior history must be purged.";
    if ctx.json {
        let payload = serde_json::json!({
            "command": "migrate genre",
            "warning": msg,
        });
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| msg.to_owned())
        );
    } else {
        eprintln!("warning: {msg}");
    }
}

/// Render the plan for dry-run + confirmation prompt.
fn render_plan(ctx: &Context, plan: &MigrationPlan, dry_run: bool) {
    if ctx.json {
        let payload = serde_json::json!({
            "command": "migrate genre",
            "status": if dry_run { "dry-run" } else { "plan" },
            "genre": plan.genre,
            "residence": plan.residence,
            "matched_paths": plan.matched_paths,
            "locations": plan.locations,
            "ignore_file": plan.ignore_file_rel,
            "orphan_branch": plan.orphan_branch,
            "noop": plan.noop,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return;
    }
    print_plan_human(plan, dry_run);
}

fn print_plan_human(plan: &MigrationPlan, dry_run: bool) {
    if dry_run {
        println!(
            "migrate genre {} → {} (DRY RUN)",
            plan.genre, plan.residence
        );
    } else {
        println!("migrate genre {} → {}", plan.genre, plan.residence);
    }
    println!("  ignore file:  {}", plan.ignore_file_rel);
    if let Some(branch) = &plan.orphan_branch {
        println!("  orphan branch: {branch}");
    }
    if plan.matched_paths.is_empty() {
        println!(
            "  matched paths: (none — either no tracked files in this genre, or already untracked)"
        );
    } else {
        println!("  matched paths ({}):", plan.matched_paths.len());
        for p in plan.matched_paths.iter().take(20) {
            println!("    - {p}");
        }
        if plan.matched_paths.len() > 20 {
            println!("    … and {} more", plan.matched_paths.len() - 20);
        }
    }
    println!("  location globs → ignore file:");
    for g in &plan.locations {
        println!("    + {g}");
    }
    if plan.noop {
        println!("  result: NOOP (nothing to change)");
    }
}

fn render_report(ctx: &Context, plan: &MigrationPlan, report: &GenreMigrationReport) {
    if ctx.json {
        let payload = serde_json::json!({
            "command": "migrate genre",
            "status": if report.noop { "noop" } else { "ok" },
            "genre": report.genre,
            "residence": report.residence,
            "ignore_file": report.ignore_file,
            "untracked_count": report.untracked_count,
            "locations_added": report.locations_added,
            "orphan_branch": report.orphan_branch,
            "commit_sha": report.commit_sha,
            "map_persisted": report.map_persisted,
            "matched_paths": plan.matched_paths,
            "noop": report.noop,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return;
    }
    if report.noop {
        println!(
            "migrate genre {}: NOOP ({})",
            report.genre, report.residence
        );
        println!(
            "  ignore file:  {}  (already governs this genre)",
            report.ignore_file
        );
        return;
    }
    println!("migrate genre {}: OK ({})", report.genre, report.residence);
    println!(
        "  untracked: {} path(s), ignore file {} ({} glob(s) added)",
        report.untracked_count, report.ignore_file, report.locations_added,
    );
    if let Some(branch) = &report.orphan_branch {
        println!("  orphan branch seeded: {branch}");
    }
    if let Some(sha) = &report.commit_sha {
        println!("  commit: {sha}");
    }
    if report.map_persisted {
        println!("  artifact-map.toml updated (audience persisted)");
    }
}

// ─── Unit tests (planner is pure — no I/O, no git) ──────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::artifact_map::{ArtifactMap, AudienceSpec, GenreSpec};

    fn sample_map() -> ArtifactMap {
        ArtifactMap::parse_toml(
            r#"
            [github-surface]
            location = ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"]
            audience = "solo"

            [chronicle]
            location = ["docs/lore/**/*.md"]
            audience = "author+agent"

            [code]
            location = ["**/*"]
            audience = "public"
            "#,
        )
        .expect("fixture parses")
    }

    #[test]
    fn noop_when_no_matches_and_audience_matches() {
        let map = sample_map();
        let spec = map
            .genres
            .iter()
            .find(|g| g.name == "github-surface")
            .cloned()
            .unwrap();
        let plan = build_plan(&spec, Path::new("/"), &[], Residence::Solo, &map);
        assert!(plan.noop);
        assert_eq!(plan.residence, "solo");
    }

    #[test]
    fn not_noop_when_audience_mismatch_even_with_no_matches() {
        let map = sample_map();
        let spec = map
            .genres
            .iter()
            .find(|g| g.name == "chronicle")
            .cloned()
            .unwrap();
        // chronicle is author+agent (=> team residence); asking for solo
        // is an intent we must honor, even if no tracked files match.
        let plan = build_plan(&spec, Path::new("/"), &[], Residence::Solo, &map);
        assert!(!plan.noop);
    }

    #[test]
    fn matched_paths_are_carried_into_plan() {
        let map = sample_map();
        let spec = map
            .genres
            .iter()
            .find(|g| g.name == "github-surface")
            .cloned()
            .unwrap();
        let matches = vec![
            "docs/surfaces/issues.md".to_owned(),
            "docs/surfaces/prs.md".to_owned(),
        ];
        let plan = build_plan(&spec, Path::new("/"), &matches, Residence::Solo, &map);
        assert_eq!(plan.matched_paths, matches);
        assert_eq!(plan.locations.len(), 3);
    }

    #[test]
    fn team_gets_orphan_branch() {
        let map = sample_map();
        let spec = map
            .genres
            .iter()
            .find(|g| g.name == "chronicle")
            .cloned()
            .unwrap();
        let plan = build_plan(&spec, Path::new("/"), &[], Residence::Team, &map);
        assert_eq!(plan.orphan_branch.as_deref(), Some("cosmon/chronicle"));
    }

    #[test]
    fn solo_has_no_orphan_branch() {
        let map = sample_map();
        let spec = map
            .genres
            .iter()
            .find(|g| g.name == "github-surface")
            .cloned()
            .unwrap();
        let plan = build_plan(&spec, Path::new("/"), &[], Residence::Solo, &map);
        assert!(plan.orphan_branch.is_none());
    }

    #[test]
    fn audience_matches_residence_truth_table() {
        let solo = GenreSpec {
            name: "x".to_owned(),
            locations: vec!["*".to_owned()],
            audience: AudienceSpec::Solo,
        };
        let public = GenreSpec {
            name: "x".to_owned(),
            locations: vec!["*".to_owned()],
            audience: AudienceSpec::Public,
        };
        assert!(audience_matches_residence(&solo, Residence::Solo));
        assert!(!audience_matches_residence(&solo, Residence::Team));
        assert!(audience_matches_residence(&public, Residence::Team));
        assert!(!audience_matches_residence(&public, Residence::Solo));
    }

    #[test]
    fn residence_literal_roundtrip() {
        for r in [
            Residence::Solo,
            Residence::Team,
            Residence::Encrypted,
            Residence::Remote,
        ] {
            let s = residence_str(r);
            let back = residence_from_str(s).unwrap();
            // Can't derive PartialEq on Residence, compare via str.
            assert_eq!(residence_str(back), s);
        }
    }
}
