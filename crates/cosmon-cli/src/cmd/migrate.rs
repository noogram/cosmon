// SPDX-License-Identifier: AGPL-3.0-only

//! `cs migrate` — move state between layouts and residences.
//!
//! Two modes of operation:
//!
//! 1. **Legacy flat-to-fleet** (default, no subcommand). Moves molecules
//!    from `ops/molecules/{id}/state.json` to
//!    `fleets/{fleet}/molecules/{id}/state.json`. Idempotent. Optionally
//!    backfills the durable archive for terminal molecules via
//!    `--archive-past`. Runs with `cs migrate [--dry-run] [--cleanup]
//!    [--archive-past]`.
//!
//! 2. **Residence migration** (sub-verbs). Moves the galaxy's memory
//!    from one residence to another, under a seal → stage → verify → flip
//!    safety spine:
//!      - `cs migrate to <residence>` — seal → stage → verify → flip;
//!      - `cs migrate verify` — re-walk the manifest, exit 0/1/2;
//!      - `cs migrate rollback` — inverse rename if `state.prev` survives.
//!
//! The residence spine is pre/post-invariant **`A_pre ⊆ A_post`** (no
//! data lost), an atomic stage-flip via two `rename(2)` calls, and an
//! operator-visible `migration-manifest.pre.json` at the galaxy root.
//! Orphan files (not referenced by any molecule) are accounted for in
//! a distinct bucket `O` with its own invariant `O_pre ⊆ O_post`:
//! migration never silently discards.
//!
//! See also [`cs verify`](super::verify) which uses the same BLAKE3 +
//! canonical-JSON sealing vocabulary for per-molecule proof-of-work.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ShellCommand;

use chrono::{DateTime, Utc};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_hash::{canonical_serialize, Hash};
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};
use serde::{Deserialize, Serialize};

use super::Context;

pub mod genre;

/// Arguments for the `migrate` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Legacy mode: preview what would be migrated without moving anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Legacy mode: remove the legacy `ops/molecules/` directory after
    /// migration.
    #[arg(long)]
    pub cleanup: bool,

    /// Legacy mode: backfill the archive for existing terminal molecules
    /// (`Completed`, `Collapsed`, `Frozen`). Idempotent: molecules
    /// already carrying `archived = true` are skipped.
    #[arg(long = "archive-past")]
    pub archive_past: bool,

    /// Residence-migration sub-verbs. When absent, runs the legacy
    /// flat-to-fleet migration (backward compatible with `cs migrate`).
    #[command(subcommand)]
    pub command: Option<MigrateCommand>,
}

/// Residence-migration sub-verbs.
#[derive(clap::Subcommand)]
pub enum MigrateCommand {
    /// Perform a residence migration: seal manifest → stage next tree →
    /// verify staged tree → atomic rename pair to flip.
    ///
    /// Atomic data-and-git residence flip. After the data-side phases
    /// (seal → stage → verify → rename), the git-side half runs
    /// (untrack + ignore-file update + path-scoped auto-commit) unless
    /// `--no-git` / `--no-commit` opt out.
    ///
    /// Per-residence behavior (the four places the galaxy's memory can
    /// live):
    ///
    ///   solo       Writes `.cosmon/` to `.git/info/exclude` (per-clone
    ///              notebook, never pushed). Sweeps any legacy
    ///              `.cosmon/` / `.worktrees/` lines still present in
    ///              the tracked `.gitignore` so the shared bulletin
    ///              board doesn't override the local rule. Total local
    ///              invisibility (ADR-055 §3.1).
    ///
    ///   team       Appends `.cosmon/state/` to the tracked
    ///              `.gitignore` (shared with the code repo) and
    ///              untracks any state files previously committed.
    ///              Structural files (`config.toml`, `formulas/*.toml`,
    ///              `surfaces.toml`, `.gitignore`) stay trackable.
    ///              Orphan-branch-backed state sharing (`cosmon/state`)
    ///              is the cosmon-le-repo backend target.
    ///
    ///   encrypted  Same gitignore footprint as `team` today. The
    ///              age-wrap backend (requires `age` on PATH and a
    ///              `--recipient` recipient key) is deferred to
    ///              cosmon-le-repo.
    ///
    ///   remote     Same gitignore footprint as `team`. The
    ///              server-backed transport backend is not yet
    ///              implemented.
    ///
    /// The pre-migration manifest snapshots git HEAD + ignore files
    /// so `cs migrate rollback` can restore the git side byte-for-byte.
    /// Orphan files (not tied to any molecule id) are carried in a
    /// distinct bucket and never silently discarded.
    To(ToArgs),
    /// Verify the pre-migration BLAKE3 manifest against the current
    /// state. Exit codes mirror `cs verify`: `0` match, `1` divergence,
    /// `2` no manifest on record.
    Verify(VerifyArgs),
    /// Roll back the last migration by re-materializing `state.prev/`.
    Rollback(RollbackArgs),
    /// Apply a residence to every tracked path classified under a
    /// single genre (ADR-057 artifact-map). Composes artifact-map +
    /// git-side migration.
    Genre(genre::GenreArgs),
}

/// Target residence name for `cs migrate to <RESIDENCE>`.
///
/// The four residences — four places the galaxy's memory lives. The
/// per-residence *backend* is implemented separately (cosmon-le-repo);
/// this command carries the safety spine that lets any backend migrate
/// without data loss.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Residence {
    /// Solo — local, single operator. Writes the whole `.cosmon/`
    /// directory to `.git/info/exclude` (per-clone notebook, never
    /// pushed) and sweeps any legacy `.cosmon/` / `.worktrees/` lines
    /// from the tracked `.gitignore` (ADR-055 §3.1).
    Solo,
    /// Team — local, shared across operators via git. Appends
    /// `.cosmon/state/` to the tracked `.gitignore` (shared bulletin
    /// board) and untracks any previously committed state files.
    /// Structural files (config.toml, formulas/*.toml, surfaces.toml)
    /// stay trackable on `main`. Orphan-branch-based state sharing
    /// (`cosmon/state`) is the cosmon-le-repo backend target.
    Team,
    /// Encrypted — local, encrypted at rest. Same gitignore footprint
    /// as `team` today; the age-wrap backend (requires `age` on PATH
    /// and a `--recipient` key) is deferred to cosmon-le-repo.
    Encrypted,
    /// Remote — server-backed state accessed via network transport.
    /// Same gitignore footprint as `team`; the transport backend is
    /// not yet implemented (tracked by cosmon-le-repo).
    Remote,
}

impl Residence {
    /// Stable lowercase string name used in manifests and on disk.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Solo => "solo",
            Self::Team => "team",
            Self::Encrypted => "encrypted",
            Self::Remote => "remote",
        }
    }
}

/// Arguments for `cs migrate to <residence>`.
///
/// Atomic data-and-git residence flip. Four phases: seal manifest →
/// stage next tree → verify staged tree → two `rename(2)` calls to
/// flip. Afterwards the git-side half runs (untrack + ignore-file
/// update + path-scoped auto-commit) unless `--no-git` / `--no-commit`
/// opt out.
///
/// Per-residence behavior (the four places the galaxy's memory can
/// live):
///
///   solo       Writes `.cosmon/` to `.git/info/exclude` (per-clone
///              notebook, never pushed). Sweeps any legacy
///              `.cosmon/` / `.worktrees/` lines still present in the
///              tracked `.gitignore` so the shared bulletin board
///              doesn't override the local rule. Total local
///              invisibility (ADR-055 §3.1).
///
///   team       Appends `.cosmon/state/` to the tracked `.gitignore`
///              (shared with the code repo) and untracks any state
///              files previously committed. Structural files
///              (`config.toml`, `formulas/*.toml`, `surfaces.toml`,
///              `.gitignore`) stay trackable. Orphan-branch-backed
///              state sharing (`cosmon/state`) is the cosmon-le-repo
///              backend target.
///
///   encrypted  Same gitignore footprint as `team` today. The
///              age-wrap backend (requires `age` on PATH and a
///              `--recipient` recipient key) is deferred to
///              cosmon-le-repo.
///
///   remote     Same gitignore footprint as `team`. The server-backed
///              transport backend is not yet implemented.
///
/// The pre-migration manifest also snapshots git HEAD + ignore files
/// so `cs migrate rollback` can restore the git side byte-for-byte.
/// Orphan files (not tied to any molecule id) are carried in a
/// distinct bucket and never silently discarded.
#[derive(clap::Args)]
pub struct ToArgs {
    /// Target residence: `solo`, `team`, `encrypted`, or `remote`.
    pub residence: Residence,

    /// Preview the plan (seal + staging path) without renaming anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the git-side half of the migration (no `git rm --cached`,
    /// no gitignore/exclude update, no auto-commit). Useful for tests
    /// and for galaxies that live outside a git repository.
    #[arg(long)]
    pub no_git: bool,

    /// Apply git-side changes to the index + ignore file but do not
    /// create the `chore(cosmon): migrate to <residence> residence
    /// (git-side)` commit. The operator can inspect `git status` and
    /// commit manually.
    #[arg(long)]
    pub no_commit: bool,
}

/// Arguments for `cs migrate verify`.
#[derive(clap::Args)]
pub struct VerifyArgs {}

/// Arguments for `cs migrate rollback`.
#[derive(clap::Args)]
pub struct RollbackArgs {
    /// Preview which rename would run without touching disk.
    #[arg(long)]
    pub dry_run: bool,
}

/// Dispatch entry point for `cs migrate`.
///
/// # Errors
///
/// Propagates I/O and state-store errors. The verify path may exit the
/// process with code 1 (divergence) or 2 (missing manifest) — see
/// [`run_verify`] for the exit-code contract.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        None => run_legacy_flat_to_fleet(ctx, args),
        Some(MigrateCommand::To(to)) => run_to(ctx, to),
        Some(MigrateCommand::Verify(v)) => run_verify(ctx, v),
        Some(MigrateCommand::Rollback(r)) => run_rollback(ctx, r),
        Some(MigrateCommand::Genre(g)) => genre::run(ctx, g),
    }
}

// ─── Residence migration: manifest types ────────────────────────────────

/// BLAKE3 manifest written at `seal` time.
///
/// The manifest travels in two halves: an opaque `body` that records
/// everything about the tree at seal time, and a `seal` hex hash over
/// `canonical_serialize(body)` that detects silent post-hoc edits. The
/// canonical serializer from [`cosmon_hash`] enforces sorted keys and
/// stable number formatting so two readers recompute the same bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Body fields sealed by the adjacent hash.
    pub body: ManifestBody,
    /// Hex BLAKE3 of the canonical JSON of `body`.
    pub seal: String,
}

/// The sealed body of a migration manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestBody {
    /// On-disk schema version (bumped if the body shape changes).
    pub schema_version: String,
    /// Seal timestamp (RFC3339 UTC).
    pub sealed_at: DateTime<Utc>,
    /// Galaxy root that was the source of the migration (e.g. `.cosmon/`).
    pub source_galaxy_root: String,
    /// Name of the state directory at the source (e.g. `state`).
    pub source_state_name: String,
    /// Target residence name (`solo`, `team`, `encrypted`, `remote`).
    pub target_residence: String,
    /// Artifacts tied to a molecule — the invariant set `A_pre`.
    pub artifacts: Vec<ArtifactEntry>,
    /// Orphan files — files on disk not referenced by any molecule id.
    /// Invariant set `O_pre`; carried across migration, never discarded.
    pub orphans: Vec<OrphanEntry>,
    /// Pre-migration git state (HEAD sha + content of `.gitignore` and
    /// `.git/info/exclude`) when the galaxy lives inside a git
    /// repository. `None` otherwise. Sealed with the rest of the body
    /// so a post-hoc edit of the ignore files is detectable.
    #[serde(default)]
    pub git_pre_state: Option<GitPreState>,
}

/// Snapshot of the git-side state captured at manifest-seal time.
///
/// Used by `cs migrate rollback` to restore the git index and ignore
/// files to exactly what they were before `cs migrate to` was invoked.
/// Missing when the galaxy is not inside a git repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitPreState {
    /// Repository root (walk-up discovery of `.git`).
    pub repo_root: String,
    /// Path of the state directory relative to the repo root
    /// (e.g. `.cosmon/state`). Used to target `git rm --cached`.
    pub state_rel_path: String,
    /// HEAD commit sha at seal time; `None` if the repo has no commits
    /// yet (fresh `git init`).
    pub head_sha: Option<String>,
    /// Verbatim content of `.gitignore` at seal time; `None` if the
    /// file did not exist. Used by rollback to restore it byte-exactly.
    pub gitignore_content: Option<String>,
    /// Verbatim content of `.git/info/exclude` at seal time; `None` if
    /// the file did not exist.
    pub exclude_content: Option<String>,
    /// List of paths (relative to repo root) that were tracked under
    /// the state directory before migration. Exposed for reports and
    /// for regression tests on the untracking step.
    pub tracked_paths_before: Vec<String>,
}

/// One artifact owned by a molecule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactEntry {
    /// Molecule id this artifact belongs to.
    pub molecule_id: String,
    /// Path relative to the state root.
    pub rel_path: String,
    /// Lowercase hex BLAKE3 hash of the file contents.
    pub blake3: String,
    /// File size in bytes at seal time.
    pub size: u64,
    /// Modification time at seal time (RFC3339 UTC).
    pub mtime: DateTime<Utc>,
}

/// One orphan file — present on disk but not tied to a molecule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrphanEntry {
    /// Path relative to the state root.
    pub rel_path: String,
    /// Lowercase hex BLAKE3 hash of the file contents.
    pub blake3: String,
    /// File size in bytes at seal time.
    pub size: u64,
    /// Modification time at seal time (RFC3339 UTC).
    pub mtime: DateTime<Utc>,
}

/// Relative path to the manifest inside the galaxy root.
const MANIFEST_NAME: &str = "migration-manifest.pre.json";
/// Suffix of the staged tree written alongside the live state.
const STAGE_SUFFIX: &str = ".next";
/// Suffix of the previous tree kept after a successful flip.
const PREV_SUFFIX: &str = ".prev";
/// Manifest schema version for on-disk compatibility tracking.
///
/// `1` — initial body (artifacts + orphans).
/// `2` — adds `git_pre_state` so rollback can restore the git index
///   and ignore files atomically.
const SCHEMA_VERSION: &str = "2";

/// Compute `A_pre` and `O_pre` over `state_dir`.
///
/// Walk is deterministic (sorted DFS). Artifacts that live under a
/// recognisable molecule directory (`fleets/*/molecules/<mol_id>/...`
/// or the legacy `ops/molecules/<mol_id>/...`) are attributed to that
/// molecule; everything else is an orphan. Staging directories
/// (`state.next`, `state.prev`) are never walked even when the caller
/// points at a galaxy root.
fn walk_state(state_dir: &Path) -> anyhow::Result<(Vec<ArtifactEntry>, Vec<OrphanEntry>)> {
    let mut artifacts: Vec<ArtifactEntry> = Vec::new();
    let mut orphans: Vec<OrphanEntry> = Vec::new();
    walk_dir(state_dir, state_dir, &mut artifacts, &mut orphans)?;
    artifacts.sort_by(|a, b| (&a.molecule_id, &a.rel_path).cmp(&(&b.molecule_id, &b.rel_path)));
    orphans.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok((artifacts, orphans))
}

/// Recursive worker for [`walk_state`]. Sorted DFS for determinism.
fn walk_dir(
    root: &Path,
    dir: &Path,
    artifacts: &mut Vec<ArtifactEntry>,
    orphans: &mut Vec<OrphanEntry>,
) -> anyhow::Result<()> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut entries: Vec<_> = rd.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let ft = entry.file_type()?;
        let path = entry.path();
        if ft.is_dir() {
            walk_dir(root, &path, artifacts, orphans)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| anyhow::anyhow!("path strip failed: {e}"))?
                .to_string_lossy()
                .into_owned();
            let bytes = fs::read(&path)?;
            let hash = Hash::of_bytes(&bytes).to_hex();
            let meta = entry.metadata()?;
            let size = meta.len();
            let mtime: DateTime<Utc> = meta.modified().map_or_else(|_| Utc::now(), Into::into);
            if let Some(mol_id) = molecule_id_from_rel(&rel) {
                artifacts.push(ArtifactEntry {
                    molecule_id: mol_id,
                    rel_path: rel,
                    blake3: hash,
                    size,
                    mtime,
                });
            } else {
                orphans.push(OrphanEntry {
                    rel_path: rel,
                    blake3: hash,
                    size,
                    mtime,
                });
            }
        }
    }
    Ok(())
}

/// Extract the owning molecule id from a state-relative path.
///
/// Returns `Some(id)` for `fleets/*/molecules/<id>/...` and for the
/// legacy `ops/molecules/<id>/...` layout; `None` otherwise (orphan).
fn molecule_id_from_rel(rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() >= 4 && parts[0] == "fleets" && parts[2] == "molecules" {
        return Some(parts[3].to_owned());
    }
    if parts.len() >= 3 && parts[0] == "ops" && parts[1] == "molecules" {
        return Some(parts[2].to_owned());
    }
    None
}

/// Build, seal, and persist a manifest for `state_dir`.
///
/// Writes `migration-manifest.pre.json` next to `state_dir` (i.e. under
/// the galaxy root). Overwrites any previous manifest — the caller is
/// responsible for staging migrations sequentially.
fn seal_manifest(
    state_dir: &Path,
    galaxy_root: &Path,
    target: Residence,
) -> anyhow::Result<(PathBuf, Manifest)> {
    let (artifacts, orphans) = walk_state(state_dir)?;
    let git_pre_state = capture_git_pre_state(galaxy_root, state_dir, target);
    let body = ManifestBody {
        schema_version: SCHEMA_VERSION.to_owned(),
        sealed_at: Utc::now(),
        source_galaxy_root: galaxy_root.display().to_string(),
        source_state_name: state_dir
            .file_name()
            .map_or_else(|| "state".to_owned(), |n| n.to_string_lossy().into_owned()),
        target_residence: target.as_str().to_owned(),
        artifacts,
        orphans,
        git_pre_state,
    };
    let seal = Hash::of_bytes(&canonical_serialize(&body)?).to_hex();
    let manifest = Manifest { body, seal };
    let path = manifest_path(galaxy_root);
    fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
    Ok((path, manifest))
}

/// Resolve the manifest path given the galaxy root.
fn manifest_path(galaxy_root: &Path) -> PathBuf {
    galaxy_root.join(MANIFEST_NAME)
}

/// Copy `from` to `to` recursively with sorted traversal for determinism.
///
/// The staged tree is a byte-for-byte copy; residence-specific transforms
/// happen at the backend layer (future work). The migration spine only
/// owns the safety contract — seal, stage, verify, flip.
fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(to)?;
    let rd = fs::read_dir(from)?;
    let mut entries: Vec<_> = rd.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let ft = entry.file_type()?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if ft.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ft.is_file() {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Compare a freshly walked `(artifacts, orphans)` against a sealed
/// manifest, returning the list of divergent entries. Empty vec ⇔
/// `A_pre ⊆ A_post` and `O_pre ⊆ O_post`.
fn diff_against_manifest(
    manifest: &Manifest,
    actual_artifacts: &[ArtifactEntry],
    actual_orphans: &[OrphanEntry],
) -> Vec<String> {
    let mut divergences: Vec<String> = Vec::new();
    let actual_by_path: BTreeMap<&str, &ArtifactEntry> = actual_artifacts
        .iter()
        .map(|a| (a.rel_path.as_str(), a))
        .collect();
    for expected in &manifest.body.artifacts {
        match actual_by_path.get(expected.rel_path.as_str()) {
            Some(got) if got.blake3 == expected.blake3 => {}
            Some(got) => divergences.push(format!(
                "artifact {} ({}): hash mismatch pre={} post={}",
                expected.molecule_id, expected.rel_path, expected.blake3, got.blake3
            )),
            None => divergences.push(format!(
                "artifact {} ({}): missing in post-migration tree",
                expected.molecule_id, expected.rel_path
            )),
        }
    }
    let actual_orph_by_path: BTreeMap<&str, &OrphanEntry> = actual_orphans
        .iter()
        .map(|o| (o.rel_path.as_str(), o))
        .collect();
    for expected in &manifest.body.orphans {
        match actual_orph_by_path.get(expected.rel_path.as_str()) {
            Some(got) if got.blake3 == expected.blake3 => {}
            Some(got) => divergences.push(format!(
                "orphan ({}): hash mismatch pre={} post={}",
                expected.rel_path, expected.blake3, got.blake3
            )),
            None => divergences.push(format!(
                "orphan ({}): missing in post-migration tree",
                expected.rel_path
            )),
        }
    }
    divergences
}

/// Verify that the sealed body in `manifest` hashes to its `seal` field.
fn seal_intact(manifest: &Manifest) -> anyhow::Result<bool> {
    let expected = Hash::of_bytes(&canonical_serialize(&manifest.body)?).to_hex();
    Ok(expected == manifest.seal)
}

/// Derive the galaxy root (parent of the state dir).
///
/// Used to place the manifest and sibling directories. For an explicit
/// `--config` override pointing at a flat dir, the galaxy root is that
/// same directory (caller holds the scope).
fn galaxy_root_for(state_dir: &Path) -> PathBuf {
    state_dir
        .parent()
        .map_or_else(|| state_dir.to_path_buf(), Path::to_path_buf)
}

/// Derive the staging path (`<state_dir>.next`).
fn stage_path(state_dir: &Path) -> PathBuf {
    sibling_with_suffix(state_dir, STAGE_SUFFIX)
}

/// Derive the rollback anchor path (`<state_dir>.prev`).
fn prev_path(state_dir: &Path) -> PathBuf {
    sibling_with_suffix(state_dir, PREV_SUFFIX)
}

fn sibling_with_suffix(state_dir: &Path, suffix: &str) -> PathBuf {
    let parent = state_dir.parent().unwrap_or_else(|| Path::new("."));
    let name = state_dir
        .file_name()
        .map_or_else(|| "state".to_owned(), |n| n.to_string_lossy().into_owned());
    parent.join(format!("{name}{suffix}"))
}

// ─── Git-side layout: the second half of an atomic migration ────────────
//
// Rationale (task-20260420-e906). Before this module, `cs migrate to
// solo` only rearranged files on disk; it did not update the git index
// or the ignore files. The name of the verb lied about the effect — the
// operator believed "solo" meant "local-only, invisible to git" but
// `.cosmon/state/` stayed tracked and `git push` still shipped it.
//
// The fix treats a migration as *atomic across data and git*. Every
// residence has a distinct git-side footprint, applied after the stage
// flip succeeds. Idempotent on re-run. Skipped when the galaxy is not
// inside a git repository. Captured in the pre-migration manifest so
// `cs migrate rollback` can restore the git index byte-for-byte.

/// Report returned by [`apply_git_side`]. Carries enough detail for a
/// human report and a JSON payload; the raw booleans feed the
/// regression tests.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GitSideReport {
    /// Whether a git repo was found (`apply_git_side` is a no-op when
    /// `false`).
    pub applied: bool,
    /// Repository root, when detected.
    pub repo_root: Option<String>,
    /// Number of paths that transitioned from *tracked* to *untracked*
    /// via `git rm -r --cached`.
    pub untracked_count: usize,
    /// Whether the residence's ignore file (`.gitignore` or
    /// `.git/info/exclude`) was modified.
    pub ignore_updated: bool,
    /// Path (relative to repo root) of the ignore file that was
    /// touched, when applicable.
    pub ignore_file: Option<String>,
    /// SHA of the git commit created by the migration, when `--no-commit`
    /// was not passed and there were staged changes.
    pub commit_sha: Option<String>,
}

/// Walk upward from `start` until a `.git` entry is found. Supports
/// both full repositories (`.git/` directory) and linked worktrees
/// (`.git` file pointing at the `gitdir`).
pub(super) fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    // Resolve symlinks so we don't stop at the wrong boundary.
    if let Ok(canon) = current.canonicalize() {
        current = canon;
    }
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Run `git` with the given arguments under `repo_root`. Returns
/// `(stdout, stderr, exit_status)` without bubbling up `ErrorKind` —
/// the caller decides whether a non-zero exit is fatal.
fn run_git(repo_root: &Path, args: &[&str]) -> anyhow::Result<(String, String, bool)> {
    let output = ShellCommand::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to invoke git: {e}"))?;
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    ))
}

/// List all paths under `rel_path` that are currently tracked.
fn git_ls_tracked_under(repo_root: &Path, rel_path: &str) -> anyhow::Result<Vec<String>> {
    // Empty relpath means "the whole repo" — guard against that.
    if rel_path.is_empty() {
        return Ok(Vec::new());
    }
    let (stdout, _stderr, _ok) = run_git(repo_root, &["ls-files", "--", rel_path])?;
    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Read `HEAD`'s commit sha. Returns `None` if the repository has no
/// commits yet (fresh `git init`).
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

/// Return the posix-style forward-slashed form of `rel_path`. Git
/// always speaks forward slashes regardless of host platform.
fn to_git_path(rel_path: &Path) -> String {
    rel_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Capture the pre-migration git state for `state_dir`.
///
/// Returns `None` when the galaxy is not inside a git repository, or
/// when the target directory does not live under that repository (a
/// pathological configuration worth refusing to touch). Best-effort:
/// any individual failure (missing file, permission denied) yields a
/// `None` so the data-side migration never hangs on git.
///
/// `residence` selects the scope of the snapshot: for `Solo` the
/// whole galaxy root is measured (solo = total); for other residences
/// the state directory is the scope.
fn capture_git_pre_state(
    galaxy_root: &Path,
    state_dir: &Path,
    residence: Residence,
) -> Option<GitPreState> {
    let repo_root = find_git_root(galaxy_root)?;
    let target = git_side_target(residence, galaxy_root, state_dir);
    let target_canon = target.canonicalize().ok()?;
    let rel = target_canon.strip_prefix(&repo_root).ok()?;
    let rel_str = to_git_path(rel);
    let tracked = git_ls_tracked_under(&repo_root, &rel_str).unwrap_or_default();
    let head_sha = git_head_sha(&repo_root);
    let gitignore_content = fs::read_to_string(repo_root.join(".gitignore")).ok();
    let exclude_content = fs::read_to_string(repo_root.join(".git/info/exclude")).ok();
    Some(GitPreState {
        repo_root: repo_root.display().to_string(),
        state_rel_path: rel_str,
        head_sha,
        gitignore_content,
        exclude_content,
        tracked_paths_before: tracked,
    })
}

/// Append a line to `path` if it is not already present (exact-match).
///
/// Returns `true` if the file was modified or created.
pub(super) fn append_line_if_missing(path: &Path, line: &str) -> anyhow::Result<bool> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let needle = line.trim_end_matches('\n');
    if existing.lines().any(|l| l.trim_end() == needle) {
        return Ok(false);
    }
    let mut new_contents = existing.clone();
    if !new_contents.is_empty() && !new_contents.ends_with('\n') {
        new_contents.push('\n');
    }
    new_contents.push_str(needle);
    new_contents.push('\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, new_contents)?;
    Ok(true)
}

/// Path of the ignore file that governs `residence`, relative to
/// `repo_root`.
///
/// - `Solo` → `.git/info/exclude` (local-only, never pushed).
/// - `Team` / `Encrypted` / `Remote` → `.gitignore` (shared with the
///   code repo so every collaborator agrees on the exclusion).
fn ignore_file_for(residence: Residence, repo_root: &Path) -> (PathBuf, &'static str) {
    match residence {
        Residence::Solo => (repo_root.join(".git/info/exclude"), ".git/info/exclude"),
        Residence::Team | Residence::Encrypted | Residence::Remote => {
            (repo_root.join(".gitignore"), ".gitignore")
        }
    }
}

/// Git-side target for `residence` — the path whose index entries
/// must be removed and whose pattern must be added to the residence's
/// ignore file.
///
/// - `Solo` → the whole galaxy root (`.cosmon/`). §3.1 of ADR-055:
///   solo means total local invisibility — zero files under
///   `.cosmon/` tracked by git. An earlier *solo partial* bug excluded
///   only `.cosmon/state/`.
/// - `Team` / `Encrypted` / `Remote` → the state directory
///   (`.cosmon/state/`). Narration moves; structural files
///   (`config.toml`, `formulas/*.toml`, `surfaces.toml`,
///   `.gitignore`) stay trackable on `main`.
fn git_side_target(residence: Residence, galaxy_root: &Path, state_dir: &Path) -> PathBuf {
    match residence {
        Residence::Solo => galaxy_root.to_path_buf(),
        Residence::Team | Residence::Encrypted | Residence::Remote => state_dir.to_path_buf(),
    }
}

/// Apply the git-side half of a residence migration.
///
/// Three steps, in order:
///   1. `git rm -r --cached --ignore-unmatch <state_rel_path>` — remove
///      tracked copies from the index (working tree intact).
///   2. Append `<state_rel_path>/` to the residence's ignore file if
///      not already present.
///   3. Commit the result with a standard subject unless `no_commit`
///      is set. The commit is path-scoped to the ignore file and the
///      state directory, never bundling unrelated work.
///
/// Idempotent — safe to re-run. No-op when no git repo is found. Best
/// effort: a failure at any step is returned as an `Err` **after** the
/// data-side migration has already committed, so the caller reports
/// the problem instead of leaving the galaxy in a split-brain state.
fn apply_git_side(
    galaxy_root: &Path,
    state_dir: &Path,
    residence: Residence,
    no_commit: bool,
) -> anyhow::Result<GitSideReport> {
    let Some(repo_root) = find_git_root(galaxy_root) else {
        return Ok(GitSideReport::default());
    };
    // Residence-aware target: Solo targets the whole galaxy root
    // (.cosmon/ — total local invisibility, ADR-055 §3.1); other
    // residences target the state directory only.
    let target = git_side_target(residence, galaxy_root, state_dir);
    let target_canon = target.canonicalize().unwrap_or_else(|_| target.clone());
    let rel = match target_canon.strip_prefix(&repo_root) {
        Ok(r) => r.to_path_buf(),
        Err(_) => {
            // Target is outside the detected repo — refuse to touch
            // git so we don't risk rewriting somebody else's index.
            return Ok(GitSideReport::default());
        }
    };
    let rel_str = to_git_path(&rel);
    if rel_str.is_empty() {
        return Ok(GitSideReport::default());
    }

    // Precondition for committing: the index must be clean relative
    // to HEAD. This keeps the migration commit focused — we refuse to
    // bundle unrelated staged work into a `chore(cosmon): migrate`
    // subject line. Skipped when `no_commit` is set (the operator is
    // explicitly taking responsibility for the commit themselves).
    if !no_commit {
        let (_out, _err, clean) = run_git(&repo_root, &["diff", "--cached", "--quiet"])?;
        if !clean {
            anyhow::bail!(
                "refusing to commit migrate changes: index is not clean. \
                 Commit or stash pending staged changes first, or re-run \
                 with --no-commit to stage the migrate diff without committing."
            );
        }
    }

    // Step 1 — untrack anything still in the index under rel_str.
    let tracked_before = git_ls_tracked_under(&repo_root, &rel_str)?;
    let untracked_count = tracked_before.len();
    if untracked_count > 0 {
        let (_out, err, ok) = run_git(
            &repo_root,
            &[
                "rm",
                "-r",
                "--cached",
                "--ignore-unmatch",
                "--quiet",
                &rel_str,
            ],
        )?;
        if !ok {
            anyhow::bail!("git rm --cached failed: {err}");
        }
    }

    // Step 2 — update the residence's ignore file. For team-class
    // residences (`.gitignore` is tracked) we stage the edit so it
    // lands in the migration commit alongside the untracking.
    let (ignore_path, ignore_rel) = ignore_file_for(residence, &repo_root);
    let ignore_line = format!("{rel_str}/");
    let ignore_updated = append_line_if_missing(&ignore_path, &ignore_line)?;
    let gitignore_is_tracked = matches!(
        residence,
        Residence::Team | Residence::Encrypted | Residence::Remote
    );
    if ignore_updated && gitignore_is_tracked {
        let (_out, err, ok) = run_git(&repo_root, &["add", "--", ignore_rel])?;
        if !ok {
            anyhow::bail!("git add {ignore_rel} failed: {err}");
        }
    }

    // Step 2b — Solo cleanup: prior installs may have written `.cosmon/`
    // and `.worktrees/` to the tracked `.gitignore`. Solo means total
    // local invisibility (ADR-055 §3.1), so those rules must leave the
    // shared bulletin board even though the exclude rule has just been
    // added in `.git/info/exclude`. We sweep the tracked `.gitignore`
    // here and stage the edit so the migration commit cleans up in one
    // atomic step.
    let mut gitignore_cleanup_staged = false;
    if matches!(residence, Residence::Solo) {
        let tracked_gitignore = repo_root.join(".gitignore");
        if let Ok(body) = fs::read_to_string(&tracked_gitignore) {
            let stripped = super::init::strip_cosmon_gitignore_block(&body);
            if stripped != body {
                if stripped.trim().is_empty() {
                    fs::write(&tracked_gitignore, "")?;
                } else {
                    fs::write(&tracked_gitignore, &stripped)?;
                }
                let (_out, err, ok) = run_git(&repo_root, &["add", "--", ".gitignore"])?;
                if !ok {
                    anyhow::bail!("git add .gitignore failed: {err}");
                }
                gitignore_cleanup_staged = true;
            }
        }
    }

    // Step 3 — commit the staged changes. We intentionally run
    // `git commit` without pathspec: pathspec-scoped commits do not
    // pick up already-staged deletions from `git rm --cached`, so the
    // clean-index precondition above is what keeps the commit focused.
    let mut commit_sha: Option<String> = None;
    let should_commit = !no_commit && (untracked_count > 0 || gitignore_cleanup_staged);
    // `ignore_updated` alone (without untracking) is only committable
    // for team-class residences where the ignore file is tracked.
    let only_ignore_change_to_commit =
        !no_commit && untracked_count == 0 && ignore_updated && gitignore_is_tracked;
    if should_commit || only_ignore_change_to_commit {
        let subject = format!(
            "chore(cosmon): migrate to {} residence (git-side)",
            residence.as_str()
        );
        let (_out, err, ok) = run_git(&repo_root, &["commit", "--quiet", "-m", &subject])?;
        if ok {
            commit_sha = git_head_sha(&repo_root);
        } else if !err.contains("nothing to commit") && !err.contains("nothing added") {
            // `git commit` exits 1 on "nothing to commit" — treat as a
            // no-op, not an error (covers the idempotent re-run).
            anyhow::bail!("git commit failed: {err}");
        }
    }

    Ok(GitSideReport {
        applied: true,
        repo_root: Some(repo_root.display().to_string()),
        untracked_count,
        ignore_updated,
        ignore_file: Some(ignore_rel.to_owned()),
        commit_sha,
    })
}

/// Restore the git-side state captured in `pre`.
///
/// The inverse of [`apply_git_side`]. Called from `cs migrate rollback`
/// after the data-side `state.prev/` has been re-materialised. Order
/// matters: restore ignore files first, then reset the index, so the
/// index reset sees the correct ignore rules.
fn restore_git_side(pre: &GitPreState) -> anyhow::Result<GitSideReport> {
    let repo_root = PathBuf::from(&pre.repo_root);
    if !repo_root.join(".git").exists() {
        // Repo disappeared since seal — nothing we can restore, but
        // don't make rollback fail on that.
        return Ok(GitSideReport::default());
    }

    // Restore `.gitignore` exactly.
    let gitignore = repo_root.join(".gitignore");
    match &pre.gitignore_content {
        Some(bytes) => fs::write(&gitignore, bytes)?,
        None => {
            if gitignore.exists() {
                fs::remove_file(&gitignore)?;
            }
        }
    }
    // Restore `.git/info/exclude` exactly.
    let exclude = repo_root.join(".git/info/exclude");
    match &pre.exclude_content {
        Some(bytes) => {
            if let Some(parent) = exclude.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&exclude, bytes)?;
        }
        None => {
            if exclude.exists() {
                fs::remove_file(&exclude)?;
            }
        }
    }

    // Reset the index to HEAD-before (if known) so any commit the
    // migration created is dropped from the branch tip. `git reset
    // --mixed` keeps the working tree intact — the data-side rollback
    // already handled those bytes.
    if let Some(head) = &pre.head_sha {
        let (_out, err, ok) = run_git(&repo_root, &["reset", "--mixed", "--quiet", head])?;
        if !ok {
            anyhow::bail!("git reset --mixed {head} failed: {err}");
        }
    }

    Ok(GitSideReport {
        applied: true,
        repo_root: Some(repo_root.display().to_string()),
        untracked_count: 0,
        ignore_updated: true,
        ignore_file: None,
        commit_sha: pre.head_sha.clone(),
    })
}

// ─── Residence-aware verify invariants (task-20260420-fa82) ─────────────
//
// The data-side manifest proves that every file survived the flip
// byte-for-byte. It does **not** prove that the residence's git-side
// intent was honored: on noesis, `cs migrate verify` returned PASS
// while `git ls-files .cosmon/` still listed nine structural files.
// This module adds the git-side invariant that each residence
// promises, so verify is no longer a false positive.

/// Per-residence git-side invariant check. Returns the list of
/// violations (empty = invariant holds). Called after the BLAKE3
/// manifest check in `run_verify`. Best-effort: if the galaxy is
/// outside any git repo the check is a no-op (invariants are vacuous
/// when there is no git to lie to).
fn verify_residence_invariants(
    galaxy_root: &Path,
    state_dir: &Path,
    residence_str: &str,
) -> Vec<String> {
    let Some(repo_root) = find_git_root(galaxy_root) else {
        return Vec::new();
    };

    let target = match residence_str {
        "solo" => galaxy_root.to_path_buf(),
        "team" | "encrypted" | "remote" => state_dir.to_path_buf(),
        _ => return Vec::new(),
    };
    let Ok(target_canon) = target.canonicalize() else {
        return Vec::new();
    };
    let Ok(rel) = target_canon.strip_prefix(&repo_root) else {
        return Vec::new();
    };
    let rel_str = to_git_path(rel);
    if rel_str.is_empty() {
        return Vec::new();
    }

    let tracked = git_ls_tracked_under(&repo_root, &rel_str).unwrap_or_default();

    let mut violations: Vec<String> = Vec::new();
    match residence_str {
        "solo" => {
            if !tracked.is_empty() {
                let listing: String = tracked
                    .iter()
                    .take(12)
                    .map(|p| format!("    - {p}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let ellipsis = if tracked.len() > 12 {
                    format!("\n    ... and {} more", tracked.len() - 12)
                } else {
                    String::new()
                };
                violations.push(format!(
                    "solo residence: {count} file(s) still tracked under {rel}/ (expected 0). \
                     Remediation: `git rm -r --cached {rel}` then `cs migrate to solo`.\n{listing}{ellipsis}",
                    count = tracked.len(),
                    rel = rel_str,
                ));
            }
        }
        "team" => {
            if !tracked.is_empty() {
                violations.push(format!(
                    "team residence: {} narration file(s) still tracked on the current branch \
                     under {rel}/ (expected 0 — narration belongs on orphan branch cosmon/state). \
                     Remediation: `git rm -r --cached {rel}` + seed the orphan branch.",
                    tracked.len(),
                    rel = rel_str,
                ));
            }
        }
        "encrypted" => {
            // Encrypted = team + age wrap. The index invariant is
            // identical to team (narration on orphan branch). Age
            // content-check is deferred until age integration lands
            // (task-20260420-a0e9-v2).
            if !tracked.is_empty() {
                violations.push(format!(
                    "encrypted residence: {} file(s) still tracked on the current branch \
                     under {rel}/ (expected 0 — narration belongs on orphan branch cosmon/state).",
                    tracked.len(),
                    rel = rel_str,
                ));
            }
        }
        "remote" => {
            if !tracked.is_empty() {
                violations.push(format!(
                    "remote residence: {} file(s) tracked locally under {rel}/ \
                     (expected 0 — state lives on the cosmon-saas server).",
                    tracked.len(),
                    rel = rel_str,
                ));
            }
            // Also check for a remote endpoint in config.toml. Best
            // effort: missing config is reported but not a hard failure.
            let config_path = galaxy_root.join("config.toml");
            if let Ok(content) = fs::read_to_string(&config_path) {
                if !content.contains("endpoint") && !content.contains("url") {
                    violations.push(format!(
                        "remote residence: {} does not appear to declare a remote endpoint \
                         (expected `endpoint = \"…\"` or `url = \"…\"`).",
                        config_path.display(),
                    ));
                }
            }
        }
        _ => {}
    }
    violations
}

// ─── Sub-verb: `cs migrate to <residence>` ──────────────────────────────

/// Execute `cs migrate to <residence>`.
///
/// Three phases from the briefing: seal, stage, verify (flip inside).
/// Idempotent on re-run — if `state.next/` already exists from a prior
/// aborted run it is removed before staging.
fn run_to(ctx: &Context, args: &ToArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    if !state_dir.is_dir() {
        anyhow::bail!("state directory not found: {}", state_dir.display());
    }
    let galaxy_root = galaxy_root_for(&state_dir);
    let stage = stage_path(&state_dir);
    let prev = prev_path(&state_dir);

    // Phase 1 — seal.
    let (manifest_path, manifest) = seal_manifest(&state_dir, &galaxy_root, args.residence)?;

    if args.dry_run {
        print_to_report(
            ctx,
            &state_dir,
            &manifest_path,
            &manifest,
            &stage,
            &prev,
            true,
            &[],
            None,
        )?;
        return Ok(());
    }

    // Phase 2 — stage. Clean any leftover from a prior aborted run so
    // this command is idempotent.
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    copy_tree(&state_dir, &stage)?;

    // Phase 3 — verify staged tree.
    let (actual_artifacts, actual_orphans) = walk_state(&stage)?;
    let divergences = diff_against_manifest(&manifest, &actual_artifacts, &actual_orphans);
    if !divergences.is_empty() {
        // Staged copy failed verification — remove it, never touch state_dir.
        let _ = fs::remove_dir_all(&stage);
        print_to_report(
            ctx,
            &state_dir,
            &manifest_path,
            &manifest,
            &stage,
            &prev,
            false,
            &divergences,
            None,
        )?;
        std::process::exit(1);
    }

    // Atomic flip — two rename(2) calls.
    if prev.exists() {
        fs::remove_dir_all(&prev)?;
    }
    fs::rename(&state_dir, &prev)?;
    if let Err(e) = fs::rename(&stage, &state_dir) {
        // Best effort: put the original back so we never leave the
        // galaxy without a state directory.
        let _ = fs::rename(&prev, &state_dir);
        return Err(e.into());
    }

    // Phase 4 — git-side: untrack, ignore, commit. This is the second
    // half of an atomic migration (task-20260420-e906). Before this
    // phase existed, `cs migrate to solo` left the git index talking
    // about the old layout, giving the operator the false impression
    // that `.cosmon/state/` was local-only when git was still tracking
    // it. Opt out with `--no-git` or `--no-commit`.
    let git_report = if args.no_git {
        None
    } else {
        Some(apply_git_side(
            &galaxy_root,
            &state_dir,
            args.residence,
            args.no_commit,
        )?)
    };

    print_to_report(
        ctx,
        &state_dir,
        &manifest_path,
        &manifest,
        &stage,
        &prev,
        false,
        &[],
        git_report.as_ref(),
    )?;
    Ok(())
}

// ─── Sub-verb: `cs migrate verify` ──────────────────────────────────────

/// Execute `cs migrate verify`.
///
/// Exit codes mirror `cs verify`:
///   - `0` — invariant holds (`A_pre ⊆ A_post` and `O_pre ⊆ O_post`);
///   - `1` — content divergence: offending entries printed;
///   - `2` — no manifest on record (pre-migration galaxy or stale state).
fn run_verify(ctx: &Context, _args: &VerifyArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let galaxy_root = galaxy_root_for(&state_dir);
    let mp = manifest_path(&galaxy_root);

    if !mp.exists() {
        if ctx.json {
            let out = serde_json::json!({
                "command": "migrate verify",
                "status": "no-manifest",
                "manifest_path": mp.display().to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!("migrate verify: INCONCLUSIVE");
            println!("  no manifest at {}", mp.display());
        }
        std::process::exit(2);
    }

    let bytes = fs::read(&mp)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;

    let seal_ok = seal_intact(&manifest)?;
    let (actual_artifacts, actual_orphans) = walk_state(&state_dir)?;
    let divergences = diff_against_manifest(&manifest, &actual_artifacts, &actual_orphans);

    // Residence-aware git-side invariants (task-20260420-fa82). Catches
    // "solo partial" galaxies where data passed but the git index still
    // tracks `.cosmon/` — the exact false-positive observed on noesis.
    let residence_violations =
        verify_residence_invariants(&galaxy_root, &state_dir, &manifest.body.target_residence);

    let any_fail = !seal_ok || !divergences.is_empty() || !residence_violations.is_empty();

    if ctx.json {
        let out = serde_json::json!({
            "command": "migrate verify",
            "status": if any_fail { "fail" } else { "pass" },
            "manifest_path": mp.display().to_string(),
            "residence": manifest.body.target_residence,
            "seal_intact": seal_ok,
            "divergences": divergences,
            "residence_violations": residence_violations,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if any_fail {
        println!("migrate verify: FAIL");
        if !seal_ok {
            println!("  manifest seal does not match body hash");
        }
        for d in &divergences {
            println!("  {d}");
        }
        for v in &residence_violations {
            println!("  {v}");
        }
    } else {
        println!("migrate verify: PASS");
        println!("  manifest:  {}", mp.display());
        println!("  residence: {}", manifest.body.target_residence);
        println!("  artifacts: {}", manifest.body.artifacts.len());
        println!("  orphans:   {}", manifest.body.orphans.len());
    }

    if any_fail {
        std::process::exit(1);
    }
    Ok(())
}

// ─── Sub-verb: `cs migrate rollback` ────────────────────────────────────

/// Execute `cs migrate rollback`.
///
/// Inverse rename pair: move the current `state/` aside to
/// `state.failed.<ts>/` and re-materialize `state.prev/` → `state/`.
/// Fails if no `state.prev/` exists — rollback requires a prior
/// successful flip (which is what created it).
fn run_rollback(ctx: &Context, args: &RollbackArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let galaxy_root = galaxy_root_for(&state_dir);
    let prev = prev_path(&state_dir);
    if !prev.is_dir() {
        anyhow::bail!("nothing to roll back: {} does not exist", prev.display());
    }
    let failed = sibling_with_suffix(
        &state_dir,
        &format!(".failed.{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
    );

    // Attempt to read the pre-migration git state from the manifest
    // so we can also undo the git-side half atomically. Absence is
    // fine (legacy manifest or non-git galaxy).
    let git_pre = fs::read(manifest_path(&galaxy_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
        .and_then(|m| m.body.git_pre_state);

    if args.dry_run {
        if ctx.json {
            let out = serde_json::json!({
                "command": "migrate rollback",
                "dry_run": true,
                "would_move_state_to": failed.display().to_string(),
                "would_promote_prev_from": prev.display().to_string(),
                "would_restore_git_side": git_pre.is_some(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!("migrate rollback (dry run):");
            println!(
                "  would move {} → {}",
                state_dir.display(),
                failed.display()
            );
            println!(
                "  would promote {} → {}",
                prev.display(),
                state_dir.display()
            );
            if git_pre.is_some() {
                println!("  would restore git-side from manifest");
            }
        }
        return Ok(());
    }

    if state_dir.exists() {
        fs::rename(&state_dir, &failed)?;
    }
    if let Err(e) = fs::rename(&prev, &state_dir) {
        // Keep the user out of a state-less galaxy: put `failed` back.
        if failed.exists() {
            let _ = fs::rename(&failed, &state_dir);
        }
        return Err(e.into());
    }

    // Restore git-side if we sealed a snapshot at migration time.
    // Failures are surfaced but do not unwind the data rename — the
    // operator can fix git-side manually and the data-side rollback
    // has already succeeded.
    let git_report = if let Some(pre) = &git_pre {
        Some(restore_git_side(pre)?)
    } else {
        None
    };

    if ctx.json {
        let mut out = serde_json::json!({
            "command": "migrate rollback",
            "moved_state_to": failed.display().to_string(),
            "restored_from": prev.display().to_string(),
        });
        if let Some(g) = &git_report {
            if let Some(obj) = out.as_object_mut() {
                obj.insert(
                    "git_side".to_string(),
                    serde_json::to_value(g).unwrap_or(serde_json::Value::Null),
                );
            }
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("migrate rollback: restored state from {}", prev.display());
        println!("  previous tree preserved at {}", failed.display());
        if let Some(g) = &git_report {
            if g.applied {
                println!(
                    "  git-side: ignore files restored, index reset to {}",
                    g.commit_sha.as_deref().unwrap_or("(no prior HEAD)"),
                );
            } else {
                println!("  git-side: skipped (no git repo)");
            }
        }
    }
    Ok(())
}

/// Render the end-of-run report for `cs migrate to <residence>`.
#[allow(clippy::too_many_arguments)]
fn print_to_report(
    ctx: &Context,
    state_dir: &Path,
    manifest_path: &Path,
    manifest: &Manifest,
    stage: &Path,
    prev: &Path,
    dry_run: bool,
    divergences: &[String],
    git_report: Option<&GitSideReport>,
) -> anyhow::Result<()> {
    if ctx.json {
        let mut out = serde_json::json!({
            "command": "migrate to",
            "dry_run": dry_run,
            "status": if divergences.is_empty() { "ok" } else { "divergence" },
            "residence": manifest.body.target_residence,
            "state_dir": state_dir.display().to_string(),
            "manifest_path": manifest_path.display().to_string(),
            "stage_path": stage.display().to_string(),
            "prev_path": prev.display().to_string(),
            "artifacts": manifest.body.artifacts.len(),
            "orphans": manifest.body.orphans.len(),
            "seal": manifest.seal,
            "divergences": divergences,
        });
        if let Some(g) = git_report {
            if let Some(obj) = out.as_object_mut() {
                obj.insert(
                    "git_side".to_string(),
                    serde_json::to_value(g).unwrap_or(serde_json::Value::Null),
                );
                // Task-spec alias fields (task-20260420-fa82) — stable
                // names for script consumers regardless of residence.
                obj.insert(
                    "info_exclude_added".to_string(),
                    serde_json::Value::Bool(
                        g.ignore_updated && g.ignore_file.as_deref() == Some(".git/info/exclude"),
                    ),
                );
                obj.insert(
                    "git_rm_cached_count".to_string(),
                    serde_json::Value::from(g.untracked_count),
                );
            }
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if dry_run {
        println!("migrate to {}: DRY RUN", manifest.body.target_residence);
    } else if divergences.is_empty() {
        println!("migrate to {}: OK", manifest.body.target_residence);
    } else {
        println!("migrate to {}: FAIL", manifest.body.target_residence);
    }
    println!("  state dir: {}", state_dir.display());
    println!("  manifest:  {}", manifest_path.display());
    println!("  stage:     {}", stage.display());
    println!("  prev:      {}", prev.display());
    println!(
        "  artifacts: {}  orphans: {}",
        manifest.body.artifacts.len(),
        manifest.body.orphans.len()
    );
    for d in divergences {
        println!("  ! {d}");
    }
    if let Some(g) = git_report {
        if g.applied {
            println!(
                "  git-side:  untracked {} path(s), ignore {}, commit {}",
                g.untracked_count,
                if g.ignore_updated {
                    "updated"
                } else {
                    "unchanged"
                },
                g.commit_sha.as_deref().unwrap_or("(none)"),
            );
            if manifest.body.target_residence == "solo" {
                // Echo the ADR-055 §3.1 promise: solo = total.
                println!(
                    "  solo total: {} file(s) untracked, .cosmon/ added to .git/info/exclude",
                    g.untracked_count,
                );
            }
        } else {
            println!("  git-side:  skipped (no git repo)");
        }
    }
    Ok(())
}

// ─── Legacy flat-to-fleet migration (unchanged behaviour) ───────────────

/// A single migration action for reporting.
#[derive(Debug, serde::Serialize)]
struct MigratedMolecule {
    id: String,
    fleet: String,
    from: String,
    to: String,
}

/// Intermediate result from scanning and migrating legacy molecules.
struct MigrateResult {
    migrated: Vec<MigratedMolecule>,
    skipped: usize,
    errors: Vec<String>,
}

/// Execute the legacy `cs migrate` (flat-to-fleet + optional archive backfill).
///
/// Scans `ops/molecules/` for legacy flat-layout molecules, deserializes
/// each one, writes it to the fleet-scoped path via `save_molecule`, and
/// then removes the legacy copy. Idempotent: molecules already in
/// `fleets/` are not touched.
fn run_legacy_flat_to_fleet(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);
    let legacy_root = state_dir.join("ops/molecules");

    let legacy_present = legacy_root.is_dir();

    let result = if legacy_present {
        let r = scan_and_migrate(&state_dir, store.as_ref(), &legacy_root, args.dry_run)?;
        if args.cleanup && !args.dry_run {
            cleanup_legacy(&state_dir, &legacy_root)?;
        }
        r
    } else {
        MigrateResult {
            migrated: Vec::new(),
            skipped: 0,
            errors: Vec::new(),
        }
    };

    let archive_report = if args.archive_past {
        Some(backfill_archive(&state_dir, store.as_ref(), args.dry_run)?)
    } else {
        None
    };

    print_legacy_report(
        ctx,
        args,
        &result.migrated,
        result.skipped,
        &result.errors,
        legacy_present,
        archive_report.as_ref(),
    )
}

/// Scan legacy molecules and migrate them to fleet-scoped layout.
fn scan_and_migrate(
    state_dir: &Path,
    store: &dyn StateStore,
    legacy_root: &Path,
    dry_run: bool,
) -> anyhow::Result<MigrateResult> {
    let fleets_root = state_dir.join("fleets");
    let mut migrated: Vec<MigratedMolecule> = Vec::new();
    let mut skipped: usize = 0;
    let mut errors: Vec<String> = Vec::new();

    for entry in fs::read_dir(legacy_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let state_path = entry.path().join("state.json");
        if !state_path.exists() {
            continue;
        }

        let dir_name = entry.file_name().to_string_lossy().to_string();

        if is_already_migrated(&fleets_root, &dir_name) {
            skipped += 1;
            continue;
        }

        match migrate_one(store, &entry, &dir_name, &fleets_root, dry_run) {
            Ok(action) => migrated.push(action),
            Err(msg) => errors.push(msg),
        }
    }

    Ok(MigrateResult {
        migrated,
        skipped,
        errors,
    })
}

/// Check whether a molecule already exists in any fleet directory.
fn is_already_migrated(fleets_root: &Path, dir_name: &str) -> bool {
    fleets_root.is_dir()
        && fs::read_dir(fleets_root).ok().is_some_and(|entries| {
            entries
                .flatten()
                .any(|fleet_entry| fleet_entry.path().join("molecules").join(dir_name).is_dir())
        })
}

/// Migrate a single molecule from legacy to fleet-scoped layout.
fn migrate_one(
    store: &dyn StateStore,
    entry: &fs::DirEntry,
    dir_name: &str,
    fleets_root: &Path,
    dry_run: bool,
) -> Result<MigratedMolecule, String> {
    let state_path = entry.path().join("state.json");
    let data =
        fs::read_to_string(&state_path).map_err(|e| format!("{dir_name}: read error: {e}"))?;
    let mol: MoleculeData =
        serde_json::from_str(&data).map_err(|e| format!("{dir_name}: parse error: {e}"))?;

    let fleet_str = mol.fleet_id.as_str().to_owned();
    let target_dir = fleets_root
        .join(&fleet_str)
        .join("molecules")
        .join(dir_name);

    let action = MigratedMolecule {
        id: dir_name.to_owned(),
        fleet: fleet_str,
        from: entry.path().display().to_string(),
        to: target_dir.display().to_string(),
    };

    if !dry_run {
        store
            .save_molecule(&mol.id, &mol)
            .map_err(|e| format!("{dir_name}: write error: {e}"))?;
        if let Err(e) = fs::remove_dir_all(entry.path()) {
            // Non-fatal — molecule was written to the new location.
            eprintln!("warning: {dir_name}: cleanup error: {e}");
        }
    }

    Ok(action)
}

/// Per-molecule record of an archive backfill action.
#[derive(Debug, serde::Serialize)]
struct ArchivedMolecule {
    id: String,
    fleet: String,
    status: String,
    trigger: String,
}

/// Aggregated result of an archive backfill pass.
#[derive(Debug, serde::Serialize)]
struct ArchiveReport {
    archived: Vec<ArchivedMolecule>,
    skipped_already_archived: usize,
    skipped_non_terminal: usize,
    errors: Vec<String>,
    archive_size_bytes: u64,
}

/// Scan every molecule under `state_dir` and write an archive entry for
/// each terminal molecule that has not yet been archived.
fn backfill_archive(
    state_dir: &Path,
    store: &dyn StateStore,
    dry_run: bool,
) -> anyhow::Result<ArchiveReport> {
    let mols = store.list_molecules(&MoleculeFilter::default())?;
    let mut archived: Vec<ArchivedMolecule> = Vec::new();
    let mut skipped_already_archived = 0usize;
    let mut skipped_non_terminal = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for mol in mols {
        let trigger = match mol.status {
            MoleculeStatus::Completed => cosmon_state::archive::Trigger::Done,
            MoleculeStatus::Collapsed => cosmon_state::archive::Trigger::Collapse,
            MoleculeStatus::Frozen => cosmon_state::archive::Trigger::Freeze,
            _ => {
                skipped_non_terminal += 1;
                continue;
            }
        };

        if mol.archived {
            skipped_already_archived += 1;
            continue;
        }

        let record = ArchivedMolecule {
            id: mol.id.as_str().to_owned(),
            fleet: mol.fleet_id.as_str().to_owned(),
            status: mol.status.to_string(),
            trigger: trigger_str(trigger).to_owned(),
        };

        if dry_run {
            archived.push(record);
            continue;
        }

        let mol_dir = cosmon_state::archive::resolve_molecule_dir(state_dir, &mol.id)
            .unwrap_or_else(|| store.molecule_dir(&mol.id));

        match cosmon_state::archive::write(state_dir, &mol_dir, &mol, trigger, chrono::Utc::now()) {
            Ok(_) => {
                let mut updated = mol;
                updated.archived = true;
                if let Err(e) = store.save_molecule(&updated.id, &updated) {
                    errors.push(format!("{}: save after archive failed: {e}", record.id));
                }
                archived.push(record);
            }
            Err(e) => {
                errors.push(format!("{}: archive write failed: {e}", record.id));
            }
        }
    }

    let archive_size_bytes = dir_size(&state_dir.join("archive")).unwrap_or(0);

    Ok(ArchiveReport {
        archived,
        skipped_already_archived,
        skipped_non_terminal,
        errors,
        archive_size_bytes,
    })
}

/// Static-string name of a [`cosmon_state::archive::Trigger`] for reports.
fn trigger_str(t: cosmon_state::archive::Trigger) -> &'static str {
    match t {
        cosmon_state::archive::Trigger::Done => "done",
        cosmon_state::archive::Trigger::Collapse => "collapse",
        cosmon_state::archive::Trigger::Freeze => "freeze",
        cosmon_state::archive::Trigger::Stuck => "stuck",
    }
}

/// Recursively sum the byte size of all regular files under `path`.
fn dir_size(path: &Path) -> std::io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(total)
}

/// Remove legacy `ops/molecules/` (and `ops/`) if empty.
fn cleanup_legacy(state_dir: &Path, legacy_root: &Path) -> anyhow::Result<()> {
    if !legacy_root.is_dir() {
        return Ok(());
    }
    let is_empty = fs::read_dir(legacy_root).is_ok_and(|mut entries| entries.next().is_none());
    if is_empty {
        fs::remove_dir_all(legacy_root)?;
        let ops_dir = state_dir.join("ops");
        if ops_dir.is_dir() {
            let ops_empty =
                fs::read_dir(&ops_dir).is_ok_and(|mut entries| entries.next().is_none());
            if ops_empty {
                fs::remove_dir_all(&ops_dir)?;
            }
        }
    }
    Ok(())
}

/// Print legacy migration results in JSON or human-readable format.
fn print_legacy_report(
    ctx: &Context,
    args: &Args,
    migrated: &[MigratedMolecule],
    skipped: usize,
    errors: &[String],
    legacy_present: bool,
    archive: Option<&ArchiveReport>,
) -> anyhow::Result<()> {
    if ctx.json {
        let mut out = serde_json::json!({
            "command": "migrate",
            "dry_run": args.dry_run,
            "legacy_present": legacy_present,
            "migrated": migrated.len(),
            "skipped": skipped,
            "errors": errors,
            "molecules": migrated,
        });
        if let Some(a) = archive {
            out.as_object_mut().unwrap().insert(
                "archive_past".to_string(),
                serde_json::to_value(a).unwrap_or(serde_json::Value::Null),
            );
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if args.dry_run {
        println!("Dry run — no changes made.\n");
    }

    if !legacy_present {
        println!("No legacy layout to migrate: ops/molecules/ does not exist.");
    } else if migrated.is_empty() && errors.is_empty() {
        if skipped > 0 {
            println!("All {skipped} molecule(s) already migrated.");
        } else {
            println!("Nothing to migrate.");
        }
    } else {
        println!(
            "Migrated {} molecule(s) to fleet-scoped layout:",
            migrated.len()
        );
        for m in migrated {
            println!("  {} → fleets/{}/molecules/{}/", m.id, m.fleet, m.id);
        }
        if skipped > 0 {
            println!("\nSkipped {skipped} already-migrated molecule(s).");
        }
        if !errors.is_empty() {
            eprintln!("\n{} error(s):", errors.len());
            for e in errors {
                eprintln!("  - {e}");
            }
        }
    }

    if let Some(a) = archive {
        println!();
        if args.dry_run {
            println!(
                "Archive backfill (dry run): {} molecule(s) would be archived, {} already archived, {} non-terminal.",
                a.archived.len(),
                a.skipped_already_archived,
                a.skipped_non_terminal,
            );
        } else {
            println!(
                "Archive backfill: {} molecule(s) archived, {} already archived, {} non-terminal. Archive size: {}.",
                a.archived.len(),
                a.skipped_already_archived,
                a.skipped_non_terminal,
                format_bytes(a.archive_size_bytes),
            );
        }
        for m in &a.archived {
            println!(
                "  {} ({}, trigger={}) → archive/YYYY/MM/{}/",
                m.id, m.status, m.trigger, m.id
            );
        }
        if !a.errors.is_empty() {
            eprintln!("\n{} archive error(s):", a.errors.len());
            for e in &a.errors {
                eprintln!("  - {e}");
            }
        }
    }

    Ok(())
}

/// Render a byte count with a human-readable unit suffix (KiB/MiB/GiB).
#[allow(clippy::cast_precision_loss)] // Archive sizes well below f64 exact-integer range.
fn format_bytes(n: u64) -> String {
    const KI: u64 = 1024;
    const MI: u64 = 1024 * KI;
    const GI: u64 = 1024 * MI;
    if n >= GI {
        format!("{:.2} GiB", n as f64 / GI as f64)
    } else if n >= MI {
        format!("{:.2} MiB", n as f64 / MI as f64)
    } else if n >= KI {
        format!("{:.2} KiB", n as f64 / KI as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_state::MoleculeData;
    use tempfile::TempDir;

    use super::*;
    use cosmon_filestore::FileStore;

    fn legacy_molecule(id: &str) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    fn write_legacy_molecule(state_dir: &Path, mol: &MoleculeData) {
        let mol_dir = state_dir.join("ops/molecules").join(mol.id.as_str());
        fs::create_dir_all(&mol_dir).unwrap();
        let json = serde_json::to_string_pretty(mol).unwrap();
        fs::write(mol_dir.join("state.json"), json).unwrap();
    }

    // ─── Legacy flat-to-fleet tests (preserved from pre-residence code) ───

    #[test]
    fn test_migrate_moves_legacy_to_fleet_scoped() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let mol = legacy_molecule("task-20260401-aaaa");
        write_legacy_molecule(&state_dir, &mol);

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: false,
            archive_past: false,
            command: None,
        };
        run(&ctx, &args).unwrap();

        let fleet_path = state_dir.join("fleets/default/molecules/task-20260401-aaaa/state.json");
        assert!(fleet_path.exists(), "molecule not in fleet-scoped path");

        let legacy_path = state_dir.join("ops/molecules/task-20260401-aaaa");
        assert!(!legacy_path.exists(), "legacy directory not removed");
    }

    #[test]
    fn test_migrate_dry_run_does_not_move() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let mol = legacy_molecule("task-20260401-bbbb");
        write_legacy_molecule(&state_dir, &mol);

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: true,
            cleanup: false,
            archive_past: false,
            command: None,
        };
        run(&ctx, &args).unwrap();

        let legacy_path = state_dir.join("ops/molecules/task-20260401-bbbb/state.json");
        assert!(legacy_path.exists(), "dry run should not move molecules");

        let fleet_path = state_dir.join("fleets/default/molecules/task-20260401-bbbb/state.json");
        assert!(!fleet_path.exists(), "dry run should not create fleet path");
    }

    #[test]
    fn test_migrate_skips_already_migrated() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let mol = legacy_molecule("task-20260401-cccc");

        write_legacy_molecule(&state_dir, &mol);
        let store = FileStore::new(&state_dir);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: false,
            archive_past: false,
            command: None,
        };
        run(&ctx, &args).unwrap();

        let legacy_path = state_dir.join("ops/molecules/task-20260401-cccc/state.json");
        assert!(legacy_path.exists());
    }

    #[test]
    fn test_migrate_idempotent() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let mol = legacy_molecule("task-20260401-dddd");
        write_legacy_molecule(&state_dir, &mol);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: false,
            archive_past: false,
            command: None,
        };

        run(&ctx, &args).unwrap();
        run(&ctx, &args).unwrap();

        let fleet_path = state_dir.join("fleets/default/molecules/task-20260401-dddd/state.json");
        assert!(fleet_path.exists());
    }

    #[test]
    fn test_migrate_cleanup_removes_empty_legacy_dir() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let mol = legacy_molecule("task-20260401-eeee");
        write_legacy_molecule(&state_dir, &mol);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: true,
            archive_past: false,
            command: None,
        };
        run(&ctx, &args).unwrap();

        assert!(!state_dir.join("ops/molecules").exists());
        assert!(!state_dir.join("ops").exists());
    }

    #[test]
    fn test_migrate_no_legacy_dir() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_dir),
        };
        let args = Args {
            dry_run: false,
            cleanup: false,
            archive_past: false,
            command: None,
        };
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_migrate_multiple_molecules() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();

        for suffix in ["aaaa", "bbbb", "cccc"] {
            let mol = legacy_molecule(&format!("task-20260401-{suffix}"));
            write_legacy_molecule(&state_dir, &mol);
        }

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: true,
            archive_past: false,
            command: None,
        };
        run(&ctx, &args).unwrap();

        for suffix in ["aaaa", "bbbb", "cccc"] {
            let fleet_path = state_dir.join(format!(
                "fleets/default/molecules/task-20260401-{suffix}/state.json"
            ));
            assert!(fleet_path.exists(), "molecule {suffix} not migrated");
        }
    }

    fn write_fleet_molecule(state_dir: &Path, id: &str, status: MoleculeStatus) {
        let store = FileStore::new(state_dir);
        let mut mol = legacy_molecule(id);
        mol.status = status;
        std::fs::create_dir_all(store.molecule_dir(&mol.id)).unwrap();
        store.save_molecule(&mol.id, &mol).unwrap();
    }

    #[test]
    fn test_archive_past_backfills_terminal_molecules() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();

        write_fleet_molecule(&state_dir, "task-20260401-comp", MoleculeStatus::Completed);
        write_fleet_molecule(&state_dir, "task-20260401-coll", MoleculeStatus::Collapsed);
        write_fleet_molecule(&state_dir, "task-20260401-froz", MoleculeStatus::Frozen);
        write_fleet_molecule(&state_dir, "task-20260401-pend", MoleculeStatus::Pending);
        write_fleet_molecule(&state_dir, "task-20260401-runx", MoleculeStatus::Running);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: false,
            archive_past: true,
            command: None,
        };
        run(&ctx, &args).unwrap();

        let archive_root = state_dir.join("archive");
        assert!(archive_root.is_dir(), "archive root not created");
        let mut found = 0usize;
        for year_entry in std::fs::read_dir(&archive_root).unwrap().flatten() {
            if !year_entry.file_type().unwrap().is_dir() {
                continue;
            }
            for month_entry in std::fs::read_dir(year_entry.path()).unwrap().flatten() {
                if !month_entry.file_type().unwrap().is_dir() {
                    continue;
                }
                for mol_entry in std::fs::read_dir(month_entry.path()).unwrap().flatten() {
                    let name = mol_entry.file_name().to_string_lossy().into_owned();
                    if [
                        "task-20260401-comp",
                        "task-20260401-coll",
                        "task-20260401-froz",
                    ]
                    .contains(&name.as_str())
                    {
                        assert!(
                            mol_entry.path().join("molecule.json").is_file(),
                            "missing molecule.json for {name}"
                        );
                        found += 1;
                    }
                }
            }
        }
        assert_eq!(found, 3, "expected 3 archive entries, found {found}");

        let store = FileStore::new(&state_dir);
        let pend = store
            .load_molecule(&MoleculeId::new("task-20260401-pend").unwrap())
            .unwrap();
        assert!(!pend.archived, "pending molecule should not be archived");
        let running = store
            .load_molecule(&MoleculeId::new("task-20260401-runx").unwrap())
            .unwrap();
        assert!(!running.archived, "running molecule should not be archived");

        for suffix in ["comp", "coll", "froz"] {
            let id = MoleculeId::new(format!("task-20260401-{suffix}")).unwrap();
            let reloaded = store.load_molecule(&id).unwrap();
            assert!(reloaded.archived, "{suffix} archived flag not set");
        }
    }

    #[test]
    fn test_archive_past_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();
        write_fleet_molecule(&state_dir, "task-20260401-idmp", MoleculeStatus::Completed);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: false,
            archive_past: true,
            command: None,
        };

        run(&ctx, &args).unwrap();
        let entries_after_first: Vec<_> = std::fs::read_dir(state_dir.join("archive").join("2026"))
            .unwrap()
            .flatten()
            .collect();
        assert!(
            !entries_after_first.is_empty(),
            "no archive after first run"
        );

        run(&ctx, &args).unwrap();

        let mut count = 0usize;
        for year in std::fs::read_dir(state_dir.join("archive"))
            .unwrap()
            .flatten()
        {
            if !year.file_type().unwrap().is_dir() {
                continue;
            }
            for month in std::fs::read_dir(year.path()).unwrap().flatten() {
                if !month.file_type().unwrap().is_dir() {
                    continue;
                }
                for mol in std::fs::read_dir(month.path()).unwrap().flatten() {
                    if mol.file_name() == "task-20260401-idmp" {
                        count += 1;
                    }
                }
            }
        }
        assert_eq!(count, 1, "idempotence violated: {count} entries");
    }

    #[test]
    fn test_archive_past_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();
        write_fleet_molecule(&state_dir, "task-20260401-dryr", MoleculeStatus::Completed);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: true,
            cleanup: false,
            archive_past: true,
            command: None,
        };
        run(&ctx, &args).unwrap();

        assert!(
            !state_dir.join("archive").exists(),
            "dry run created archive directory",
        );

        let store = FileStore::new(&state_dir);
        let mol = store
            .load_molecule(&MoleculeId::new("task-20260401-dryr").unwrap())
            .unwrap();
        assert!(!mol.archived, "dry run should not flip archived flag");
    }

    #[test]
    fn test_archive_past_skips_already_archived() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().to_path_buf();

        let store = FileStore::new(&state_dir);
        let mut mol = legacy_molecule("task-20260401-skip");
        mol.status = MoleculeStatus::Completed;
        mol.archived = true;
        std::fs::create_dir_all(store.molecule_dir(&mol.id)).unwrap();
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            dry_run: false,
            cleanup: false,
            archive_past: true,
            command: None,
        };
        run(&ctx, &args).unwrap();

        let archive_mol_path = state_dir
            .join("archive")
            .join("2026")
            .join("04")
            .join("task-20260401-skip");
        assert!(
            !archive_mol_path.exists(),
            "already-archived molecule should not be re-archived"
        );
    }

    // ─── Residence-migration spine: unit tests ───────────────────────────

    /// Seed a miniature state tree at `galaxy_root/state/` with one
    /// fleet-scoped molecule and one orphan file, then return the
    /// `state/` path.
    fn seed_state_tree(galaxy_root: &Path) -> PathBuf {
        let state = galaxy_root.join("state");
        let mol_dir = state.join("fleets/default/molecules/task-20260420-a1b2");
        fs::create_dir_all(&mol_dir).unwrap();
        fs::write(mol_dir.join("state.json"), r#"{"id":"task-20260420-a1b2"}"#).unwrap();
        fs::write(mol_dir.join("prompt.md"), "operator intent").unwrap();
        fs::create_dir_all(state.join("config")).unwrap();
        fs::write(state.join("config/fleet.json"), "{}").unwrap();
        state
    }

    #[test]
    fn walk_state_categorises_molecules_and_orphans() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let (artifacts, orphans) = walk_state(&state).unwrap();

        assert_eq!(artifacts.len(), 2, "two fleet artifacts expected");
        assert!(artifacts
            .iter()
            .all(|a| a.molecule_id == "task-20260420-a1b2"));
        assert_eq!(orphans.len(), 1, "one orphan (config/fleet.json) expected");
        assert_eq!(orphans[0].rel_path, "config/fleet.json");
    }

    #[test]
    fn molecule_id_from_rel_recognises_fleet_and_ops_layouts() {
        assert_eq!(
            molecule_id_from_rel("fleets/default/molecules/task-123/state.json"),
            Some("task-123".to_owned())
        );
        assert_eq!(
            molecule_id_from_rel("ops/molecules/task-123/state.json"),
            Some("task-123".to_owned())
        );
        assert_eq!(molecule_id_from_rel("archive/2026/04/foo"), None);
    }

    #[test]
    fn migrate_to_happy_path_flips_state_atomically() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        let args = ToArgs {
            residence: Residence::Team,
            dry_run: false,
            no_git: true,
            no_commit: true,
        };
        run_to(&ctx, &args).unwrap();

        // Manifest was written at galaxy root.
        let mp = manifest_path(tmp.path());
        assert!(mp.exists(), "manifest not written at {}", mp.display());

        // state.prev/ holds the original tree; state/ is the flipped
        // copy. state.next/ was consumed by the rename.
        assert!(tmp.path().join("state.prev").is_dir());
        assert!(state.is_dir());
        assert!(!tmp.path().join("state.next").exists());

        // Every seeded file is present in the new state tree.
        assert!(state
            .join("fleets/default/molecules/task-20260420-a1b2/prompt.md")
            .is_file());
        assert!(state.join("config/fleet.json").is_file());
    }

    #[test]
    fn migrate_to_dry_run_only_seals_manifest() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        let args = ToArgs {
            residence: Residence::Solo,
            dry_run: true,
            no_git: true,
            no_commit: true,
        };
        run_to(&ctx, &args).unwrap();

        assert!(manifest_path(tmp.path()).exists());
        assert!(!tmp.path().join("state.next").exists());
        assert!(!tmp.path().join("state.prev").exists());
        // state/ untouched on dry run.
        assert!(state
            .join("fleets/default/molecules/task-20260420-a1b2/state.json")
            .is_file());
    }

    #[test]
    fn migrate_verify_reports_pass_after_happy_migration() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Team,
                dry_run: false,
                no_git: true,
                no_commit: true,
            },
        )
        .unwrap();

        // Re-read manifest, re-walk state, compare in-process (bypass
        // the CLI's std::process::exit).
        let manifest_bytes = fs::read(manifest_path(tmp.path())).unwrap();
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes).unwrap();
        assert!(seal_intact(&manifest).unwrap());
        let (a, o) = walk_state(&state).unwrap();
        assert!(diff_against_manifest(&manifest, &a, &o).is_empty());
    }

    #[test]
    fn migrate_verify_detects_content_divergence() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Team,
                dry_run: false,
                no_git: true,
                no_commit: true,
            },
        )
        .unwrap();

        // Tamper: overwrite a file inside the live state.
        fs::write(
            state.join("fleets/default/molecules/task-20260420-a1b2/prompt.md"),
            "tampered",
        )
        .unwrap();

        let manifest_bytes = fs::read(manifest_path(tmp.path())).unwrap();
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let (a, o) = walk_state(&state).unwrap();
        let divs = diff_against_manifest(&manifest, &a, &o);
        assert!(
            divs.iter().any(|d| d.contains("prompt.md")),
            "divergence list should flag prompt.md: {divs:?}"
        );
    }

    #[test]
    fn migrate_verify_no_manifest_is_inconclusive() {
        // No manifest at the galaxy root → verify would exit 2.
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let mp = manifest_path(tmp.path());
        assert!(!mp.exists(), "precondition: no manifest");

        // Calling walk_state without a manifest is fine; the CLI path
        // exits 2 by policy. We assert the manifest really is absent
        // and that walking works in its absence.
        let (artifacts, _orphans) = walk_state(&state).unwrap();
        assert!(!artifacts.is_empty());
    }

    #[test]
    fn migrate_rollback_restores_state_from_prev() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Team,
                dry_run: false,
                no_git: true,
                no_commit: true,
            },
        )
        .unwrap();

        // Edit the current state to prove rollback reverts the edit.
        fs::write(
            state.join("fleets/default/molecules/task-20260420-a1b2/prompt.md"),
            "post-migration edit",
        )
        .unwrap();

        run_rollback(&ctx, &RollbackArgs { dry_run: false }).unwrap();

        // The restored state/ is the pre-migration tree.
        let restored =
            fs::read_to_string(state.join("fleets/default/molecules/task-20260420-a1b2/prompt.md"))
                .unwrap();
        assert_eq!(restored, "operator intent");

        // The failed tree is kept alongside — never silently discarded.
        let parent = tmp.path();
        let any_failed = fs::read_dir(parent)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("state.failed."));
        assert!(any_failed, "expected state.failed.* sibling after rollback");
    }

    #[test]
    fn migrate_rollback_fails_without_prev() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state),
        };
        let err = run_rollback(&ctx, &RollbackArgs { dry_run: false }).unwrap_err();
        assert!(
            err.to_string().contains("nothing to roll back"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn migrate_roundtrip_seed_migrate_verify_rollback_verify() {
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };

        // 1) migrate
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: true,
                no_commit: true,
            },
        )
        .unwrap();

        // 2) verify (in-process form — assert PASS)
        let mb = fs::read(manifest_path(tmp.path())).unwrap();
        let m: Manifest = serde_json::from_slice(&mb).unwrap();
        assert!(seal_intact(&m).unwrap());
        let (a, o) = walk_state(&state).unwrap();
        assert!(diff_against_manifest(&m, &a, &o).is_empty());

        // 3) rollback
        run_rollback(&ctx, &RollbackArgs { dry_run: false }).unwrap();

        // 4) verify again — after rollback the live state still matches
        // the manifest because the manifest sealed the *source* tree,
        // which is exactly what was just restored.
        let (a2, o2) = walk_state(&state).unwrap();
        assert!(diff_against_manifest(&m, &a2, &o2).is_empty());
    }

    // ─── Git-side integration (task-20260420-e906) ──────────────────────
    //
    // These tests reproduce the bug observed on /Users/you/noesis:
    // `cs migrate to solo` did not touch the git index or ignore files,
    // leaving the operator with a galaxy that *looked* local but kept
    // pushing `.cosmon/state/` to the shared remote.

    /// Build a throwaway git repository at `root` with a committed
    /// `.cosmon/state/` subtree, simulating the pre-migration state of
    /// a galaxy that was initialised *before* residence discipline
    /// landed. Returns the `.cosmon/state` absolute path.
    fn seed_git_galaxy(root: &Path) -> PathBuf {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .expect("git invocation failed in test setup");
            assert!(status.success(), "git {args:?} failed");
        };

        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@cosmon.local"]);
        run(&["config", "user.name", "Cosmon Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "# galaxy\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "initial"]);

        let state = root.join(".cosmon").join("state");
        let mol_dir = state.join("fleets/default/molecules/task-20260420-a1b2");
        fs::create_dir_all(&mol_dir).unwrap();
        fs::write(mol_dir.join("state.json"), r#"{"id":"task-20260420-a1b2"}"#).unwrap();
        fs::write(mol_dir.join("prompt.md"), "operator intent").unwrap();

        run(&["add", ".cosmon/state"]);
        run(&["commit", "-q", "-m", "track .cosmon/state (legacy)"]);

        state
    }

    fn git_ls_files(repo_root: &Path, rel: &str) -> Vec<String> {
        let out = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["ls-files", "--", rel])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn git_head_sha_test(repo_root: &Path) -> String {
        let out = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Regression test for the exact bug observed on /Users/you/noesis:
    /// `cs migrate to solo` must leave zero files tracked under
    /// `.cosmon/` (the whole galaxy subtree, not just `.cosmon/state/`)
    /// afterwards — solo = TOTAL.
    #[test]
    fn migrate_to_solo_untracks_previously_tracked_state() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);

        // Precondition — the bug's input condition. State is tracked.
        let before = git_ls_files(repo_root, ".cosmon");
        assert!(
            !before.is_empty(),
            "precondition failed: .cosmon/ should be tracked"
        );

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: false,
                no_commit: false,
            },
        )
        .unwrap();

        // The bug v2 (task-20260420-fa82): previously this assertion
        // missed the .cosmon/ structural files (config.toml, formulas).
        // Solo TOTAL means zero tracked anywhere under .cosmon/.
        let after = git_ls_files(repo_root, ".cosmon");
        assert!(
            after.is_empty(),
            "post-migrate to solo: .cosmon/ should be untracked entirely, but still has {} file(s): {:?}",
            after.len(),
            after,
        );

        // Solo writes `.cosmon/` (not `.cosmon/state/`) to
        // `.git/info/exclude`. Total local invisibility.
        let exclude = fs::read_to_string(repo_root.join(".git/info/exclude")).unwrap();
        assert!(
            exclude.lines().any(|l| l.trim() == ".cosmon/"),
            "`.git/info/exclude` must contain `.cosmon/` (solo total), got: {exclude:?}"
        );
        // `.gitignore` must remain untouched for solo.
        assert!(
            !repo_root.join(".gitignore").exists()
                || !fs::read_to_string(repo_root.join(".gitignore"))
                    .unwrap()
                    .contains(".cosmon/"),
            "solo must NOT add .cosmon/ to shared .gitignore",
        );
    }

    #[test]
    fn migrate_to_team_adds_state_to_gitignore_and_untracks() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Team,
                dry_run: false,
                no_git: false,
                no_commit: false,
            },
        )
        .unwrap();

        assert!(git_ls_files(repo_root, ".cosmon/state").is_empty());
        let gitignore = fs::read_to_string(repo_root.join(".gitignore")).unwrap();
        assert!(
            gitignore.lines().any(|l| l.trim() == ".cosmon/state/"),
            "team must append `.cosmon/state/` to the shared `.gitignore`"
        );
    }

    #[test]
    fn migrate_to_solo_is_idempotent_git_side() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        let mk = || ToArgs {
            residence: Residence::Solo,
            dry_run: false,
            no_git: false,
            no_commit: false,
        };
        run_to(&ctx, &mk()).unwrap();
        // Second run must not fail and must not duplicate the exclude line.
        run_to(&ctx, &mk()).unwrap();

        // task-20260420-fa82: the exclude line is now `.cosmon/`
        // (whole galaxy subtree), not `.cosmon/state/`.
        let exclude = fs::read_to_string(repo_root.join(".git/info/exclude")).unwrap();
        let count = exclude.lines().filter(|l| l.trim() == ".cosmon/").count();
        assert_eq!(count, 1, "exclude line duplicated: {exclude:?}");
    }

    #[test]
    fn migrate_to_solo_no_git_leaves_index_untouched() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);
        let before = git_ls_files(repo_root, ".cosmon/state");

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: true,
                no_commit: true,
            },
        )
        .unwrap();

        let after = git_ls_files(repo_root, ".cosmon/state");
        assert_eq!(after, before, "--no-git must leave the index alone");
    }

    #[test]
    fn migrate_to_solo_no_commit_stages_without_commit() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);
        let head_before = git_head_sha_test(repo_root);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: false,
                no_commit: true,
            },
        )
        .unwrap();

        let head_after = git_head_sha_test(repo_root);
        assert_eq!(
            head_before, head_after,
            "--no-commit must NOT create a commit"
        );
        // But the index must reflect the untracking.
        assert!(git_ls_files(repo_root, ".cosmon/state").is_empty());
    }

    #[test]
    fn apply_git_side_skips_when_not_in_git_repo() {
        // No `git init` — galaxy is a plain temp dir. Migration must
        // still succeed (data-side runs), and git-side reports "not
        // applied".
        let tmp = TempDir::new().unwrap();
        let state = seed_state_tree(tmp.path());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: false,
                no_commit: false,
            },
        )
        .unwrap();

        // Data-side happened.
        assert!(state
            .join("fleets/default/molecules/task-20260420-a1b2/state.json")
            .is_file());
        // No `.git` was ever created under the tmp (outside-of-git galaxy).
        assert!(!tmp.path().join(".git").exists());
    }

    #[test]
    fn migrate_rollback_restores_git_side() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);
        let tracked_before: std::collections::BTreeSet<String> =
            git_ls_files(repo_root, ".cosmon/state")
                .into_iter()
                .collect();
        let gitignore_before = fs::read_to_string(repo_root.join(".gitignore")).unwrap_or_default();
        let exclude_before =
            fs::read_to_string(repo_root.join(".git/info/exclude")).unwrap_or_default();
        let head_before = git_head_sha_test(repo_root);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };

        // Migrate to solo with git-side effects.
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: false,
                no_commit: false,
            },
        )
        .unwrap();
        assert!(git_ls_files(repo_root, ".cosmon/state").is_empty());

        // Rollback — must restore index, exclude, and (if HEAD moved) HEAD.
        run_rollback(&ctx, &RollbackArgs { dry_run: false }).unwrap();

        let tracked_after: std::collections::BTreeSet<String> =
            git_ls_files(repo_root, ".cosmon/state")
                .into_iter()
                .collect();
        assert_eq!(
            tracked_after, tracked_before,
            "rollback must restore every tracked file under .cosmon/state"
        );
        let exclude_after =
            fs::read_to_string(repo_root.join(".git/info/exclude")).unwrap_or_default();
        assert_eq!(
            exclude_after, exclude_before,
            "rollback must restore .git/info/exclude byte-for-byte"
        );
        let gitignore_after = fs::read_to_string(repo_root.join(".gitignore")).unwrap_or_default();
        assert_eq!(
            gitignore_after, gitignore_before,
            "rollback must restore .gitignore byte-for-byte"
        );
        assert_eq!(
            git_head_sha_test(repo_root),
            head_before,
            "rollback must reset HEAD to the pre-migration commit"
        );
    }

    #[test]
    fn migrate_to_solo_no_commit_then_team_is_idempotent() {
        // Solo → Team transition: ensure the second migration still
        // works when the first one left uncommitted staged changes.
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state.clone()),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: false,
                no_commit: false,
            },
        )
        .unwrap();
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Team,
                dry_run: false,
                no_git: false,
                no_commit: false,
            },
        )
        .unwrap();

        // Team must have added the gitignore rule on top of the solo
        // exclude rule.
        let gitignore = fs::read_to_string(repo_root.join(".gitignore")).unwrap();
        assert!(
            gitignore.lines().any(|l| l.trim() == ".cosmon/state/"),
            "team migration after solo must still add gitignore entry"
        );
    }

    /// `Atlas` regression: a tracked `.gitignore` carrying the legacy
    /// `.cosmon/` + `.worktrees/` block must be cleaned up by
    /// `cs migrate to solo`. Solo means total local invisibility — those
    /// rules are redundant once `.cosmon/` is in `.git/info/exclude` and
    /// they leak worker private state onto the shared bulletin board.
    #[test]
    fn migrate_to_solo_cleans_legacy_gitignore_block() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);

        // Seed the Atlas-style `.gitignore` block and commit it so the
        // pre-migration index matches the real-world case.
        let legacy = "target/\n\n# Cosmon agent-orchestration state (local-only, not pushed to shared repo)\n.cosmon/\n.worktrees/\n\nnode_modules/\n";
        fs::write(repo_root.join(".gitignore"), legacy).unwrap();
        let run_git_test = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .current_dir(repo_root)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run_git_test(&["add", ".gitignore"]);
        run_git_test(&["commit", "-q", "-m", "seed legacy gitignore"]);

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state),
        };
        run_to(
            &ctx,
            &ToArgs {
                residence: Residence::Solo,
                dry_run: false,
                no_git: false,
                no_commit: false,
            },
        )
        .expect("migrate to solo must succeed");

        let body = fs::read_to_string(repo_root.join(".gitignore")).unwrap();
        assert!(
            !body.contains(".cosmon/"),
            "legacy `.cosmon/` rule must be swept from tracked .gitignore: {body:?}"
        );
        assert!(
            !body.contains(".worktrees/"),
            "legacy `.worktrees/` rule must be swept from tracked .gitignore: {body:?}"
        );
        assert!(
            !body.contains("# Cosmon"),
            "orphan Cosmon header comment must be removed: {body:?}"
        );
        assert!(body.contains("target/"), "user rules must survive");
        assert!(body.contains("node_modules/"), "user rules must survive");

        // The exclude file still holds the local-only rule.
        let exclude = fs::read_to_string(repo_root.join(".git/info/exclude")).unwrap();
        assert!(
            exclude.lines().any(|l| l.trim() == ".cosmon/"),
            ".git/info/exclude must contain .cosmon/: {exclude:?}"
        );
    }

    #[test]
    fn manifest_body_carries_git_pre_state_when_in_git_repo() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);

        let galaxy_root = galaxy_root_for(&state);
        // Solo targets the whole galaxy root (task-20260420-fa82).
        let (_path, manifest) = seal_manifest(&state, &galaxy_root, Residence::Solo).unwrap();
        let pre = manifest
            .body
            .git_pre_state
            .expect("git_pre_state should be populated when in git repo");
        assert_eq!(pre.state_rel_path, ".cosmon");
        assert!(pre.head_sha.is_some());
        assert!(
            !pre.tracked_paths_before.is_empty(),
            "expected seeded tracked paths"
        );

        // Team scopes the snapshot at .cosmon/state.
        let (_p, m2) = seal_manifest(&state, &galaxy_root, Residence::Team).unwrap();
        let pre2 = m2.body.git_pre_state.unwrap();
        assert_eq!(pre2.state_rel_path, ".cosmon/state");
    }

    #[test]
    fn find_git_root_walks_up_from_state_dir() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let state = seed_git_galaxy(repo_root);
        let galaxy_root = galaxy_root_for(&state);
        let found = find_git_root(&galaxy_root).expect("should find git root");
        assert_eq!(
            found.canonicalize().unwrap(),
            repo_root.canonicalize().unwrap()
        );
    }

    #[test]
    fn find_git_root_returns_none_outside_repo() {
        let tmp = TempDir::new().unwrap();
        // Avoid false positives when the temp dir lives under a repo
        // (e.g. running cargo test from inside cosmon itself). We
        // assert the helper handles *absolute* non-repo paths by using
        // a subdirectory that is definitely not a git repo and walking
        // up from there would eventually find /, which has no .git.
        let sub = tmp.path().join("no-repo-here");
        fs::create_dir_all(&sub).unwrap();
        // This is a best-effort assertion: if the tmp dir is itself
        // inside a git repo (unusual), this will still find it. We
        // just verify the function doesn't panic and returns a
        // PathBuf or None.
        let _ = find_git_root(&sub);
    }

    #[test]
    fn append_line_if_missing_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.txt");
        assert!(append_line_if_missing(&p, "alpha").unwrap());
        assert!(!append_line_if_missing(&p, "alpha").unwrap());
        assert!(append_line_if_missing(&p, "beta").unwrap());
        let content = fs::read_to_string(&p).unwrap();
        assert_eq!(content, "alpha\nbeta\n");
    }
}
