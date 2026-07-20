// SPDX-License-Identifier: AGPL-3.0-only

//! `cs sync` — worktree-side base-sync (delib-20260720-cff4, Phase 1).
//!
//! When main advances fast, a worker resyncs its base *inside* the molecule's
//! worktree, before `cs done`, so conflicts are resolved where the worker
//! still has the context to resolve them rather than at harvest time in the
//! main checkout (ADR-052 §D5-bis). Until now that was a raw `git merge main`,
//! whose subject git writes itself — and the *direction heuristic* (`main→feat`
//! vs `feat→main`) was the only thing distinguishing that base-sync from a
//! completion merge (torvalds, delib-20260720-cff4).
//!
//! `cs sync` performs that merge under cosmon's control and stamps an explicit
//! [`Base-Sync`](super::lineage::BASE_SYNC_KEY) trailer naming the
//! `<base>..<branch>` span. The trailer is a durable, unambiguous marker: the
//! provenance gate no longer has to *infer* base-sync from a subject string it
//! does not own. The subject is kept in git's historical shape
//! (`Merge branch 'main' into feat/<mol>`) so the existing gate's structural
//! check keeps recognising it, and the new trailer only *hardens* recognition
//! — it never relaxes the safety requirement that the incoming side already
//! sit on the trunk's first-parent chain.
//!
//! This is a worktree-side verb, not a change to the frozen `cs done` surface
//! (§8p). ADR-052 §D5-bis named exactly this owner ("`cs sync`, emitting a
//! cosmon-controlled subject") as the correct home for the gesture.

use std::path::Path;
use std::process::Command;

use super::lineage;
use super::Context;

/// Arguments for `cs sync`.
#[derive(clap::Args)]
pub struct Args {
    /// Base branch to sync from (defaults to `main`).
    #[arg(long, default_value = "main")]
    pub base: String,

    /// Report what would happen without performing the merge.
    #[arg(long)]
    pub dry_run: bool,
}

/// Execute `cs sync`.
pub fn run(_ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let repo_root = repo_root()?;
    let branch = current_branch(&repo_root)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve current branch (detached HEAD?)"))?;

    if branch == args.base {
        anyhow::bail!(
            "already on the base branch `{}` — `cs sync` runs inside a feat/<mol> worktree",
            args.base
        );
    }

    let subject = format!("Merge branch '{}' into {branch}", args.base);
    let trailer = lineage::base_sync_trailer(&args.base, &branch);

    if args.dry_run {
        println!("would base-sync {}..{branch}", args.base);
        println!("  subject: {subject}");
        println!("  trailer: {trailer}");
        return Ok(());
    }

    // `--no-ff` guarantees a merge commit that can carry the trailer even when
    // the branch could fast-forward. `-m subject -m trailer` renders the
    // trailer as its own contiguous paragraph (a trailer block must not be
    // split from the subject only by the -m boundary, so we pass two -m).
    let status = Command::new("git")
        .env("LC_ALL", "C")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "merge",
            "--no-ff",
            "-m",
            &subject,
            "-m",
            &trailer,
            &args.base,
        ])
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run git merge: {e}"))?;

    if !status.success() {
        anyhow::bail!(
            "base-sync merge failed or hit a conflict; resolve it in this worktree, \
             then commit (the Base-Sync trailer subject is `{subject}`)"
        );
    }

    println!("✅ base-synced {}..{branch}", args.base);
    Ok(())
}

/// Repository root via `git rev-parse --show-toplevel`.
fn repo_root() -> anyhow::Result<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git: {e}"))?;
    if !output.status.success() {
        anyhow::bail!("not inside a git repository");
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(std::path::PathBuf::from(path))
}

/// Current branch name, or `None` on detached HEAD.
fn current_branch(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if name.is_empty() || name == "HEAD" {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_builds_expected_subject_and_trailer() {
        // Pure string assembly, independent of a repo.
        let base = "main";
        let branch = "feat/task-20260720-cccc";
        let subject = format!("Merge branch '{base}' into {branch}");
        assert_eq!(subject, "Merge branch 'main' into feat/task-20260720-cccc");
        assert_eq!(
            lineage::base_sync_trailer(base, branch),
            "Base-Sync: main..feat/task-20260720-cccc"
        );
    }
}
