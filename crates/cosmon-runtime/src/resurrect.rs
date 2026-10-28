// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule resurrection — the part of `cs resurrect` and
//! `POST /v1/molecules/:id/resurrect` that does not depend on how a worker
//! is spawned.
//!
//! Resurrection is the operation that re-attaches an observer (a fresh
//! worker) to a molecule whose previous worker crashed. The molecule never
//! died — only the observer was lost. This module
//! houses the **pure** half of the machinery: a single function that reads
//! a molecule directory and composes the bootstrap prompt for the new
//! worker. Around it, [`prepare`] checks the preconditions and takes the
//! resurrection lock, and [`ResurrectPlan::record`] leaves the breadcrumb and
//! the events. Both callers run those two and differ only in the spawn between
//! them: the CLI through its tmux path, the RPP route through
//! [`crate::tackle_exec::LibraryExecutor::resurrect`].
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
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// A resurrection-specific error surface.
///
/// `#[non_exhaustive]` so adding `BranchDiverged` / `ArtifactsCorrupt` in
/// v1.1 is a minor-version change (tolnay).
#[derive(Debug, Error)]
#[non_exhaustive]
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
    /// The molecule was dispatched through a pinned Codex Exec launch, which
    /// has no continuation contract an unpinned Claude worker could honour.
    #[error("resurrect: pinned Codex Exec has no qualified continuation contract; resolve explicitly before a new dispatch")]
    PinnedExec,
    /// The molecule could not be loaded or the state store failed.
    #[error("state store error: {0}")]
    State(String),
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

/// What [`prepare`] established: everything a spawn needs, plus the lock that
/// keeps a second resurrection out until the plan is dropped.
#[derive(Debug)]
pub struct ResurrectPlan {
    /// The wrecked molecule, as loaded.
    pub molecule: MoleculeData,
    /// Canonical molecule directory.
    pub mol_dir: PathBuf,
    /// Branch of the molecule's worktree.
    pub branch: String,
    /// Session name the new worker reuses.
    pub session_name: String,
    /// Resurrections already recorded for this molecule.
    pub prior_count: u32,
    /// The composed bootstrap prompt.
    pub prompt: String,
    /// Byte length of [`Self::prompt`].
    pub composed_prompt_bytes: u64,
    wrecks_dir: PathBuf,
    _lock: fs::File,
}

/// Check the resurrection preconditions, take the resurrection lock and
/// compose the bootstrap prompt. Mutates nothing but the lock file.
///
/// Preconditions, all of which must hold: the molecule is not pinned to a
/// Codex Exec launch; it is `Frozen`; `prompt.md` and `briefing.md` exist; no
/// worker session is alive (`session_alive`, asked of the session name the new
/// worker would reuse); and no other resurrection holds the lock.
///
/// # Errors
///
/// A [`ResurrectError`] naming the first precondition that failed.
pub fn prepare(
    store: &FileStore,
    state_dir: &Path,
    id: &MoleculeId,
    repo_root: &Path,
    session_alive: &dyn Fn(&str) -> bool,
) -> Result<ResurrectPlan, ResurrectError> {
    let mol = store
        .load_molecule(id)
        .map_err(|e| ResurrectError::State(e.to_string()))?;
    if cosmon_state::transport_pin::pinned_molecules(state_dir)?.contains(id) {
        return Err(ResurrectError::PinnedExec);
    }
    // Frozen is the wreck state produced by `cs recover`.
    if mol.status != MoleculeStatus::Frozen {
        return Err(ResurrectError::NotAWreck {
            molecule_id: id.as_str().to_owned(),
            status: mol.status.to_string(),
        });
    }
    let mol_dir = store.molecule_dir(id);
    for required in ["prompt.md", "briefing.md"] {
        if !mol_dir.join(required).exists() {
            return Err(ResurrectError::ArtifactsMissing {
                mol_dir: mol_dir.clone(),
                missing: required.to_owned(),
            });
        }
    }

    let branch = mol
        .originating_branch
        .clone()
        .unwrap_or_else(|| format!("feat/{}", id.as_str()));
    let session_name = mol
        .session_name
        .clone()
        .unwrap_or_else(|| id.as_str().to_owned());
    if session_alive(&session_name) {
        return Err(ResurrectError::DoubleResurrect {
            molecule_id: id.as_str().to_owned(),
            session: session_name,
        });
    }

    // Rejects a concurrent second invocation.
    fs::create_dir_all(&mol_dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(mol_dir.join("resurrect.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| ResurrectError::FlockContended {
            molecule_id: id.as_str().to_owned(),
        })?;

    // One breadcrumb file per resurrection: the filesystem is the count.
    let wrecks_dir = mol_dir.join("wrecks");
    let prior_count = count_prior_wrecks(&wrecks_dir);
    let prompt = compose_resurrection_prompt(
        &mol_dir,
        &ComposeContext {
            molecule_id: id.as_str(),
            branch: &branch,
            repo_root,
            prior_count,
            next_step_display: Some(format!("Step {}/{}", mol.current_step + 1, mol.total_steps)),
            synthesis_is_draft: false,
        },
    )?;
    let composed_prompt_bytes = prompt.len() as u64;
    Ok(ResurrectPlan {
        molecule: mol,
        mol_dir,
        branch,
        session_name,
        prior_count,
        prompt,
        composed_prompt_bytes,
        wrecks_dir,
        _lock: lock,
    })
}

impl ResurrectPlan {
    /// Leave the breadcrumb and the events of a resurrection that spawned.
    ///
    /// `prior_session` is the session the molecule held *before* the
    /// resurrection. The breadcrumb is best-effort; the events are too, as
    /// everywhere on the event log.
    pub fn record(&self, state_dir: &Path, repo_root: &Path, prior_session: Option<String>) {
        let prompt_hash = {
            let mut h = Sha256::new();
            h.update(self.prompt.as_bytes());
            format!("{:x}", h.finalize())
        };
        let tip_sha = resolve_tip_sha(repo_root, &self.branch);
        if let Err(e) = write_breadcrumb(
            &self.wrecks_dir,
            &tip_sha,
            self.prior_count,
            &prompt_hash,
            self.composed_prompt_bytes,
        ) {
            eprintln!("warn: failed to write resurrect breadcrumb: {e}");
        }
        let events_path = state_dir.join("events.jsonl");
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            EventV2::Resurrected {
                molecule_id: self.molecule.id.clone(),
                from_session: prior_session,
                composed_prompt_bytes: self.composed_prompt_bytes,
                t_orig_tokens: None,
                prior_count: self.prior_count,
            },
            None,
        );
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            EventV2::MoleculeStatusChanged {
                molecule_id: self.molecule.id.clone(),
                from: "frozen".to_owned(),
                to: "running".to_owned(),
            },
            None,
        );
    }
}

/// Number of resurrections already recorded under `wrecks_dir`.
#[must_use]
pub fn count_prior_wrecks(wrecks_dir: &Path) -> u32 {
    let Ok(rd) = fs::read_dir(wrecks_dir) else {
        return 0;
    };
    let n = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn resolve_tip_sha(repo_root: &Path, branch: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", branch])
        .current_dir(repo_root)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        _ => String::new(),
    }
}

fn write_breadcrumb(
    wrecks_dir: &Path,
    tip_sha: &str,
    prior_count: u32,
    prompt_hash: &str,
    prompt_bytes: u64,
) -> std::io::Result<PathBuf> {
    fs::create_dir_all(wrecks_dir)?;
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let path = wrecks_dir.join(format!("{ts}.json"));
    let payload = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(),
        "tip_sha": tip_sha,
        "prior_count": prior_count,
        "composed_prompt_hash": prompt_hash,
        "composed_prompt_bytes": prompt_bytes,
        "t_orig_tokens": serde_json::Value::Null,
    });
    let mut f = fs::File::create(&path)?;
    f.write_all(serde_json::to_string_pretty(&payload)?.as_bytes())?;
    Ok(path)
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

    #[test]
    fn count_prior_wrecks_handles_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(count_prior_wrecks(&tmp.path().join("does-not-exist")), 0);
    }

    #[test]
    fn count_prior_wrecks_counts_only_json_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wrecks");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("one.json"), "{}").unwrap();
        fs::write(dir.join("two.json"), "{}").unwrap();
        fs::write(dir.join("note.txt"), "ignore").unwrap();
        assert_eq!(count_prior_wrecks(&dir), 2);
    }

    #[test]
    fn write_breadcrumb_produces_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wrecks");
        let p = write_breadcrumb(&dir, "abc123", 1, "deadbeef", 2048).unwrap();
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["tip_sha"], "abc123");
        assert_eq!(v["prior_count"], 1);
        assert_eq!(v["composed_prompt_hash"], "deadbeef");
        assert_eq!(v["composed_prompt_bytes"], 2048);
    }
}
