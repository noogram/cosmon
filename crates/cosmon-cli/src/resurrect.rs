// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule resurrection — pure prompt composition for `cs resurrect`.
//!
//! Resurrection is the operation that re-attaches an observer (a fresh
//! worker) to a molecule whose previous worker crashed. The molecule never
//! died — only the observer was lost. This module
//! houses the **pure** half of the machinery: a single function that reads
//! a molecule directory and composes the bootstrap prompt for the new
//! worker. CLI wiring (tmux spawn, status flip, event emission) lives in
//! `cosmon-cli::cmd::resurrect`.
//!
//! The prompt is composed in causal order (shannon's "directed lossy code"):
//!
//! 1. System preamble — you are resuming; do not redo completed steps.
//! 2. `prompt.md` verbatim (original operator intent).
//! 3. `briefing.md` verbatim (the plan).
//! 4. `git log --first-parent` since nucleation — the gated-bits receipt.
//! 5. Last 3 entries of `log.md` (the worker's own log).
//! 6. `synthesis.md` (marked DRAFT when events.jsonl disagrees).
//! 7. Directory listing of `responses/*.md` (filenames only).
//! 8. Footer — timestamp, `prior_count`, current step target.
//!
//! Git log replaces `events.jsonl` as the causal receipt: it is dense,
//! gated (a commit means a step actually landed), and chronological.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// A resurrection-specific error surface.
///
/// `#[non_exhaustive]` so adding `BranchDiverged` / `ArtifactsCorrupt` in
/// v1.1 is a minor-version change (tolnay).
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(dead_code)]
pub enum ResurrectError {
    /// The target molecule is not in a wreck state — refuse to act.
    #[error("molecule {molecule_id} is not a wreck (status={status}); resurrection is only valid for stuck molecules")]
    NotAWreck {
        /// Molecule ID.
        molecule_id: String,
        /// Current status string.
        status: String,
    },
    /// The tmux session for this molecule is still alive — resurrecting
    /// would spawn a competing worker on the same worktree.
    #[error("tmux session {session} for molecule {molecule_id} is still alive; resurrection would race the running worker")]
    DoubleResurrect {
        /// Molecule ID.
        molecule_id: String,
        /// Session that is still alive.
        session: String,
    },
    /// One or more load-bearing artifacts are missing from the molecule
    /// directory. The composer cannot build a bootstrap prompt without them.
    #[error("artifacts missing in {mol_dir}: {missing}")]
    ArtifactsMissing {
        /// The molecule directory.
        mol_dir: PathBuf,
        /// Comma-separated list of missing filenames.
        missing: String,
    },
    /// Another resurrection is already in progress (flock contended).
    #[error("another `cs resurrect` invocation is in progress for molecule {molecule_id}")]
    FlockContended {
        /// Molecule ID.
        molecule_id: String,
    },
    /// Transport backend (tmux) error.
    #[error("transport error: {0}")]
    Transport(String),
    /// Filesystem / I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Inputs for [`compose_resurrection_prompt`] that aren't filesystem-resident.
///
/// Separated into a struct so the core composer function stays pure and the
/// CLI wrapper can mock or override these in tests without touching the
/// filesystem contract.
#[derive(Debug, Clone)]
pub struct ComposeContext<'a> {
    /// Molecule identifier (e.g. `task-20260414-a1b2`).
    pub molecule_id: &'a str,
    /// The git branch the molecule's worktree is on (for `git log`).
    pub branch: &'a str,
    /// The repository root — `git -C <root> log ...` is run against this.
    pub repo_root: &'a Path,
    /// Prior resurrection count (number of `Resurrected` events already
    /// in the log for this molecule). `0` on first resurrection.
    pub prior_count: u32,
    /// Number of the next incomplete step in the checklist (1-indexed,
    /// matches the CLI's display). When `None` the footer just says
    /// "see briefing.md".
    pub next_step_display: Option<String>,
    /// Whether events.jsonl disagrees with synthesis.md acceptance —
    /// when `true`, the synthesis section is flagged "DRAFT / unverified".
    pub synthesis_is_draft: bool,
}

/// Compose a bootstrap prompt for a fresh worker reviving a wrecked molecule.
///
/// **Pure-ish** — the function reads files from `mol_dir` and invokes
/// `git log` via `Command`, but it performs no state mutation. Safe to
/// call multiple times (idempotent).
///
/// The composer refuses to proceed when `prompt.md` or `briefing.md` is
/// missing — those are the ground truth of operator intent and cannot
/// be reconstructed. Other artifacts (log.md, synthesis.md, responses/)
/// are optional and simply skipped when absent.
///
/// # Errors
///
/// Returns [`ResurrectError::ArtifactsMissing`] when `prompt.md` or
/// `briefing.md` cannot be read; [`ResurrectError::Io`] on other
/// filesystem errors.
pub fn compose_resurrection_prompt(
    mol_dir: &Path,
    ctx: &ComposeContext<'_>,
) -> Result<String, ResurrectError> {
    let prompt_md = read_required(mol_dir, "prompt.md")?;
    let briefing_md = read_required(mol_dir, "briefing.md")?;

    let mut out = String::with_capacity(prompt_md.len() + briefing_md.len() + 2048);

    // 1. System preamble.
    writeln!(
        out,
        "# 🚨 COSMON RESURRECTION — RESUMING WRECKED MOLECULE 🚨\n\n\
         You are a **fresh worker** resuming cosmon molecule `{}`. The\n\
         previous worker crashed or its context window was lost. The\n\
         filesystem has preserved the work it judged load-bearing. Use\n\
         the artifacts below to continue from where the previous session\n\
         stopped. **Do NOT redo completed steps.** The step checklist in\n\
         the briefing tells you the next action.\n",
        ctx.molecule_id
    )
    .ok();

    // 2. Original intent.
    out.push_str("\n## 1. Original intent (prompt.md)\n\n");
    out.push_str(&prompt_md);
    if !prompt_md.ends_with('\n') {
        out.push('\n');
    }

    // 3. Plan.
    out.push_str("\n## 2. Plan (briefing.md)\n\n");
    out.push_str(&briefing_md);
    if !briefing_md.ends_with('\n') {
        out.push('\n');
    }

    // 4. Git log — the causal receipt.
    out.push_str("\n## 3. Completed work (git log, first-parent)\n\n");
    let commits = git_log_first_parent(ctx.repo_root, ctx.branch);
    if commits.trim().is_empty() {
        out.push_str("_(no commits on branch — step 1 may not yet be complete)_\n");
    } else {
        out.push_str("```\n");
        out.push_str(&commits);
        if !commits.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }

    // 5. Worker log (last 3 entries).
    if let Some(tail) = read_log_tail(mol_dir, 3) {
        out.push_str("\n## 4. Worker log (last 3 entries of log.md)\n\n");
        out.push_str(&tail);
        if !tail.ends_with('\n') {
            out.push('\n');
        }
    }

    // 6. Synthesis.
    if let Ok(syn) = std::fs::read_to_string(mol_dir.join("synthesis.md")) {
        if ctx.synthesis_is_draft {
            out.push_str("\n## 5. Partial synthesis (DRAFT / unverified)\n\n");
            out.push_str(
                "> ⚠ events.jsonl does not confirm acceptance of the\n\
                 > corresponding step — treat the following as an\n\
                 > in-progress draft.\n\n",
            );
        } else {
            out.push_str("\n## 5. Partial synthesis (synthesis.md)\n\n");
        }
        out.push_str(&syn);
        if !syn.ends_with('\n') {
            out.push('\n');
        }
    }

    // 7. Prior cognition artifacts (filenames only).
    if let Some(resp_list) = list_responses(mol_dir) {
        if !resp_list.is_empty() {
            out.push_str("\n## 6. Prior cognition artifacts (read on demand)\n\n");
            for name in &resp_list {
                let _ = writeln!(out, "- responses/{name}");
            }
        }
    }

    // 8. Footer.
    out.push_str("\n## Footer\n\n");
    let _ = writeln!(
        out,
        "- Resumed from wreck at {}",
        chrono::Utc::now().to_rfc3339()
    );
    let _ = writeln!(out, "- Prior resurrection count: {}", ctx.prior_count);
    if let Some(ref step) = ctx.next_step_display {
        let _ = writeln!(out, "- Current step target: {step}");
    } else {
        out.push_str("- Current step target: see briefing.md checklist\n");
    }
    out.push_str(
        "- When you complete a step, run \
         `cs evolve <id> --evidence \"<summary>\"`.\n",
    );

    Ok(out)
}

fn read_required(mol_dir: &Path, name: &str) -> Result<String, ResurrectError> {
    match std::fs::read_to_string(mol_dir.join(name)) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ResurrectError::ArtifactsMissing {
                mol_dir: mol_dir.to_path_buf(),
                missing: name.to_owned(),
            })
        }
        Err(e) => Err(ResurrectError::Io(e)),
    }
}

fn git_log_first_parent(repo_root: &Path, branch: &str) -> String {
    // `--first-parent` keeps the branch lineage; `--format` gives the short
    // sha + subject on one line.
    let output = Command::new("git")
        .args([
            "log",
            "--first-parent",
            "--format=%h %ad %s",
            "--date=short",
            branch,
        ])
        .current_dir(repo_root)
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

fn read_log_tail(mol_dir: &Path, n: usize) -> Option<String> {
    let text = std::fs::read_to_string(mol_dir.join("log.md")).ok()?;
    // A log entry is delimited by a heading-ish line starting with `## `
    // or `- `. We keep it simple: tail the last `n * ~20` lines so the
    // context window isn't blown up. Workers can read the full file if
    // they want more.
    let lines: Vec<&str> = text.lines().collect();
    let keep = lines.len().min(n * 30);
    let start = lines.len().saturating_sub(keep);
    Some(lines[start..].join("\n"))
}

fn list_responses(mol_dir: &Path) -> Option<Vec<String>> {
    let responses = mol_dir.join("responses");
    let entries = std::fs::read_dir(&responses).ok()?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                p.file_name().map(|n| n.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_repo(dir: &Path) {
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(dir)
            .status()
            .unwrap();
        fs::write(dir.join("README"), "seed").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "seed"])
            .current_dir(dir)
            .status()
            .unwrap();
    }

    fn basic_ctx<'a>(repo: &'a Path, mol_id: &'a str, branch: &'a str) -> ComposeContext<'a> {
        ComposeContext {
            molecule_id: mol_id,
            branch,
            repo_root: repo,
            prior_count: 0,
            next_step_display: Some("Step 2/3: Verify".to_owned()),
            synthesis_is_draft: false,
        }
    }

    #[test]
    fn test_compose_requires_prompt_md() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("briefing.md"), "plan").unwrap();
        let ctx = basic_ctx(tmp.path(), "task-x", "HEAD");
        let err = compose_resurrection_prompt(tmp.path(), &ctx).unwrap_err();
        assert!(matches!(err, ResurrectError::ArtifactsMissing { .. }));
    }

    #[test]
    fn test_compose_requires_briefing_md() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("prompt.md"), "intent").unwrap();
        let ctx = basic_ctx(tmp.path(), "task-x", "HEAD");
        let err = compose_resurrection_prompt(tmp.path(), &ctx).unwrap_err();
        assert!(matches!(err, ResurrectError::ArtifactsMissing { .. }));
    }

    #[test]
    fn test_compose_resurrection_prompt_orders_artifacts_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let mol_dir = tmp.path().join("mol");
        fs::create_dir_all(&mol_dir).unwrap();
        fs::write(mol_dir.join("prompt.md"), "ORIGINAL INTENT").unwrap();
        fs::write(mol_dir.join("briefing.md"), "PLAN BODY").unwrap();
        fs::write(mol_dir.join("log.md"), "- entry one\n- entry two\n").unwrap();
        fs::write(mol_dir.join("synthesis.md"), "SYNTH BODY").unwrap();
        fs::create_dir_all(mol_dir.join("responses")).unwrap();
        fs::write(mol_dir.join("responses/a.md"), "x").unwrap();
        fs::write(mol_dir.join("responses/b.md"), "y").unwrap();

        let ctx = basic_ctx(tmp.path(), "task-x", "HEAD");
        let out = compose_resurrection_prompt(&mol_dir, &ctx).unwrap();

        // Required ordering: intent → plan → git log → log → synthesis → responses → footer.
        let i_intent = out.find("ORIGINAL INTENT").expect("intent present");
        let i_plan = out.find("PLAN BODY").expect("plan present");
        let i_gitlog = out.find("Completed work").expect("git log header");
        let i_log = out.find("entry two").expect("log present");
        let i_syn = out.find("SYNTH BODY").expect("synthesis present");
        let i_resp = out.find("responses/a.md").expect("response listed");
        let i_footer = out
            .find("Prior resurrection count")
            .expect("footer present");

        assert!(i_intent < i_plan);
        assert!(i_plan < i_gitlog);
        assert!(i_gitlog < i_log);
        assert!(i_log < i_syn);
        assert!(i_syn < i_resp);
        assert!(i_resp < i_footer);
    }

    #[test]
    fn test_compose_resurrection_prompt_marks_synthesis_as_draft_when_events_disagree() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let mol_dir = tmp.path().join("mol");
        fs::create_dir_all(&mol_dir).unwrap();
        fs::write(mol_dir.join("prompt.md"), "intent").unwrap();
        fs::write(mol_dir.join("briefing.md"), "plan").unwrap();
        fs::write(mol_dir.join("synthesis.md"), "draft body").unwrap();

        let mut ctx = basic_ctx(tmp.path(), "task-x", "HEAD");
        ctx.synthesis_is_draft = true;
        let out = compose_resurrection_prompt(&mol_dir, &ctx).unwrap();
        assert!(out.contains("DRAFT / unverified"));
        assert!(out.contains("draft body"));
    }

    #[test]
    fn test_compose_succeeds_without_optional_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let mol_dir = tmp.path().join("mol");
        fs::create_dir_all(&mol_dir).unwrap();
        fs::write(mol_dir.join("prompt.md"), "intent").unwrap();
        fs::write(mol_dir.join("briefing.md"), "plan").unwrap();

        let ctx = basic_ctx(tmp.path(), "task-x", "HEAD");
        let out = compose_resurrection_prompt(&mol_dir, &ctx).unwrap();
        assert!(out.contains("intent"));
        assert!(out.contains("plan"));
        assert!(out.contains("Prior resurrection count: 0"));
    }
}
