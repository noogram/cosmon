// SPDX-License-Identifier: AGPL-3.0-only

//! `cs diverge` — structural agreement check between two cosmon sessions.
//!
//! The agreement relation is decidable:
//!
//! ```text
//! agree(S_i, S_j, M) :=
//!     view(M).state         == view(M).state
//!  && view(M).current_step  == view(M).current_step
//!  && view(M).briefing_seals == view(M).briefing_seals
//!  && git_merge_base(a.head, b.head) is non-empty
//! ```
//!
//! Each clause is `O(1)` or `O(log history)`; the whole check runs in
//! bounded time. **Structural agreement only** — semantic agreement is
//! undecidable (Rice). Use this as a divergence detector, not a
//! correctness proof.
//!
//! ## Session resolution
//!
//! A session argument may be:
//!
//! 1. A **session id** resolved against the canonical presence registry at
//!    `<state>/presence/<sid>/presence.json` (schema: `cwd`, `galaxy`,
//!    `current_molecule`, …). This is the path C-PRESENCE-CORE publishes.
//! 2. A **filesystem path** to a galaxy root (a directory that contains a
//!    `.cosmon/` subdirectory). Used for ad-hoc checks between worktrees
//!    before the presence registry is populated.
//!
//! Both forms land on the same triple: `(state_dir, cwd, molecule_hint)`.
//!
//! ## Exit codes
//!
//! - `0` — sessions agree on every comparable clause (and the molecule
//!   exists on both sides).
//! - `1` — at least one clause disagrees. The diff summary names the
//!   disagreeing clauses.
//! - `2` — inconclusive: a session cannot be resolved, the molecule is
//!   absent on one side, or a `git` probe failed. A caller that treats
//!   "unknown" and "disagree" differently should branch on this code.

use std::path::{Path, PathBuf};
use std::process::Command;

use colored::Colorize;
use cosmon_core::id::MoleculeId;
use cosmon_state::MoleculeData;
use serde::{Deserialize, Serialize};

use super::Context;

/// Arguments for the `diverge` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// First session — a session id or a path to a galaxy root.
    pub a: String,

    /// Second session — a session id or a path to a galaxy root.
    pub b: String,

    /// Molecule id (or prefix) whose views to compare. If omitted, only
    /// the git merge-base clause is evaluated and all molecule clauses
    /// are marked inconclusive.
    #[arg(long, short = 'm')]
    pub molecule: Option<String>,
}

/// Outcome of a structural agreement check.
///
/// Three-valued so callers can distinguish "provably disagree" from
/// "we don't know". See module docs §Exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agreement {
    /// Every comparable clause holds.
    Agree,
    /// At least one clause disagrees.
    Diverge,
    /// A session, molecule, or probe was unresolvable.
    Inconclusive,
}

impl Agreement {
    /// Unix-style exit code: `0 | 1 | 2`.
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Agree => 0,
            Self::Diverge => 1,
            Self::Inconclusive => 2,
        }
    }
}

/// Minimal schema of the presence record that this command reads.
///
/// Mirrors the shape C-PRESENCE-CORE will publish at
/// `.cosmon/state/presence/<sid>/presence.json`. Unknown fields are
/// tolerated so later additions don't break this reader. Defined here
/// (rather than imported from `cosmon-state`) because the presence crate
/// lands in a sibling task and this command must not block on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Presence {
    /// Working directory (absolute path) of the session — used both as
    /// the git-HEAD probe target and (when it contains a `.cosmon/`) as
    /// the state-dir source for the session's molecule views.
    cwd: PathBuf,
    /// Optional hint: the molecule this session is currently working on.
    /// Consumed by `cs patrol --livelock` when building the wait graph.
    #[serde(default)]
    current_molecule: Option<String>,
}

/// Fully resolved view of a session — the triple `(state_dir, cwd, hint)`.
struct Session {
    /// User-facing label — original `<a>`/`<b>` argument for diagnostics.
    label: String,
    /// Path to the `.cosmon/state` dir. Used to load `MoleculeData`.
    state_dir: PathBuf,
    /// Working directory — `git rev-parse HEAD` is executed here.
    cwd: PathBuf,
}

/// Run the `cs diverge` command.
///
/// # Errors
/// Bubbles any I/O or JSON error. A *disagreement* is not an error —
/// it is the successful return of `Ok(())` with a non-zero exit code
/// applied before returning.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let a = resolve_session(ctx, &args.a)?;
    let b = resolve_session(ctx, &args.b)?;

    let mol_id = args
        .molecule
        .as_deref()
        .map(resolve_molecule_prefix)
        .transpose()?;

    let report = compute_report(&a, &b, mol_id.as_ref());

    let outcome = match report.outcome.as_str() {
        "agree" => Agreement::Agree,
        "diverge" => Agreement::Diverge,
        _ => Agreement::Inconclusive,
    };

    if ctx.json {
        let json = serde_json::to_string_pretty(&report)?;
        println!("{json}");
    } else {
        print_human(&report);
    }

    std::process::exit(outcome.exit_code());
}

/// JSON-stable report body.
#[derive(Debug, Serialize)]
pub struct DivergeReport {
    /// Left session label (as the user typed it).
    pub a: String,
    /// Right session label.
    pub b: String,
    /// Molecule id (if one was supplied).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub molecule: Option<String>,
    /// Per-clause verdicts.
    pub clauses: Vec<ClauseVerdict>,
    /// Overall outcome — `Agree | Diverge | Inconclusive`.
    pub outcome: String,
}

/// One clause of the structural agreement predicate.
#[derive(Debug, Serialize)]
pub struct ClauseVerdict {
    /// `state | current_step | briefing_seals | git_merge_base`.
    pub clause: String,
    /// `agree | diverge | inconclusive`.
    pub verdict: String,
    /// Human summary (e.g., `"running vs completed"`).
    pub detail: String,
}

#[derive(Debug)]
struct InternalReport {
    a: String,
    b: String,
    molecule: Option<String>,
    clauses: Vec<ClauseVerdict>,
    outcome: Agreement,
}

impl InternalReport {
    fn into_public(self) -> DivergeReport {
        DivergeReport {
            a: self.a,
            b: self.b,
            molecule: self.molecule,
            clauses: self.clauses,
            outcome: match self.outcome {
                Agreement::Agree => "agree",
                Agreement::Diverge => "diverge",
                Agreement::Inconclusive => "inconclusive",
            }
            .to_owned(),
        }
    }
}

/// Append the three molecule-view clauses to `clauses`.
fn append_molecule_clauses(
    a: &Session,
    b: &Session,
    mol: Option<&MoleculeId>,
    clauses: &mut Vec<ClauseVerdict>,
    any_diverge: &mut bool,
    any_inconclusive: &mut bool,
) {
    let Some(m) = mol else {
        *any_inconclusive = true;
        for clause in ["state", "current_step", "briefing_seals"] {
            clauses.push(ClauseVerdict {
                clause: clause.to_owned(),
                verdict: "inconclusive".to_owned(),
                detail: "no --molecule supplied".to_owned(),
            });
        }
        return;
    };

    let loaded_a = load_molecule(a, m);
    let loaded_b = load_molecule(b, m);
    let (Ok(ma), Ok(mb)) = (&loaded_a, &loaded_b) else {
        *any_inconclusive = true;
        let detail = format!(
            "{}={}, {}={}",
            a.label,
            describe_molecule_result(&loaded_a),
            b.label,
            describe_molecule_result(&loaded_b),
        );
        for clause in ["state", "current_step", "briefing_seals"] {
            clauses.push(ClauseVerdict {
                clause: clause.to_owned(),
                verdict: "inconclusive".to_owned(),
                detail: detail.clone(),
            });
        }
        return;
    };

    let state_v = verdict_eq("state", &ma.status.to_string(), &mb.status.to_string());
    if state_v.verdict == "diverge" {
        *any_diverge = true;
    }
    clauses.push(state_v);

    let step_v = verdict_eq(
        "current_step",
        &ma.current_step.to_string(),
        &mb.current_step.to_string(),
    );
    if step_v.verdict == "diverge" {
        *any_diverge = true;
    }
    clauses.push(step_v);

    let seals_v = verdict_briefing_seals(ma, mb);
    if seals_v.verdict == "diverge" {
        *any_diverge = true;
    }
    clauses.push(seals_v);
}

/// Append the git `merge-base` clause to `clauses`.
fn append_git_clause(
    a: &Session,
    b: &Session,
    clauses: &mut Vec<ClauseVerdict>,
    any_diverge: &mut bool,
    any_inconclusive: &mut bool,
) {
    let (Some(ha), Some(hb)) = (git_head(&a.cwd), git_head(&b.cwd)) else {
        *any_inconclusive = true;
        clauses.push(ClauseVerdict {
            clause: "git_merge_base".to_owned(),
            verdict: "inconclusive".to_owned(),
            detail: "one or both cwd are not a git repository".to_owned(),
        });
        return;
    };
    if let Some(base) = git_merge_base(&a.cwd, &ha, &hb) {
        clauses.push(ClauseVerdict {
            clause: "git_merge_base".to_owned(),
            verdict: "agree".to_owned(),
            detail: format!("merge-base = {}", short_sha(&base)),
        });
    } else {
        *any_diverge = true;
        clauses.push(ClauseVerdict {
            clause: "git_merge_base".to_owned(),
            verdict: "diverge".to_owned(),
            detail: format!(
                "no common ancestor between {} and {}",
                short_sha(&ha),
                short_sha(&hb)
            ),
        });
    }
}

fn compute_report(a: &Session, b: &Session, mol: Option<&MoleculeId>) -> DivergeReport {
    let mut clauses: Vec<ClauseVerdict> = Vec::new();
    let mut any_diverge = false;
    let mut any_inconclusive = false;

    append_molecule_clauses(
        a,
        b,
        mol,
        &mut clauses,
        &mut any_diverge,
        &mut any_inconclusive,
    );
    append_git_clause(a, b, &mut clauses, &mut any_diverge, &mut any_inconclusive);

    let outcome = if any_diverge {
        Agreement::Diverge
    } else if any_inconclusive {
        Agreement::Inconclusive
    } else {
        Agreement::Agree
    };

    InternalReport {
        a: a.label.clone(),
        b: b.label.clone(),
        molecule: mol.map(MoleculeId::to_string),
        clauses,
        outcome,
    }
    .into_public()
}

fn describe_molecule_result(r: &anyhow::Result<MoleculeData>) -> &'static str {
    if r.is_ok() {
        "ok"
    } else {
        "not found"
    }
}

fn verdict_eq(clause: &str, a: &str, b: &str) -> ClauseVerdict {
    if a == b {
        ClauseVerdict {
            clause: clause.to_owned(),
            verdict: "agree".to_owned(),
            detail: a.to_owned(),
        }
    } else {
        ClauseVerdict {
            clause: clause.to_owned(),
            verdict: "diverge".to_owned(),
            detail: format!("{a} vs {b}"),
        }
    }
}

fn verdict_briefing_seals(a: &MoleculeData, b: &MoleculeData) -> ClauseVerdict {
    let hashes_a: Vec<&str> = a.briefing_seals.iter().map(|s| s.hash.as_str()).collect();
    let hashes_b: Vec<&str> = b.briefing_seals.iter().map(|s| s.hash.as_str()).collect();
    if hashes_a == hashes_b {
        return ClauseVerdict {
            clause: "briefing_seals".to_owned(),
            verdict: "agree".to_owned(),
            detail: format!("{} seal(s) match", hashes_a.len()),
        };
    }
    ClauseVerdict {
        clause: "briefing_seals".to_owned(),
        verdict: "diverge".to_owned(),
        detail: format!("{} vs {} seal(s)", hashes_a.len(), hashes_b.len()),
    }
}

fn print_human(r: &DivergeReport) {
    let header = match r.outcome.as_str() {
        "agree" => "✓ AGREE".green().bold(),
        "diverge" => "✗ DIVERGE".red().bold(),
        _ => "? INCONCLUSIVE".yellow().bold(),
    };
    println!(
        "{} — sessions {} vs {}{}",
        header,
        r.a,
        r.b,
        r.molecule
            .as_ref()
            .map(|m| format!(" on {m}"))
            .unwrap_or_default()
    );
    for c in &r.clauses {
        let mark = match c.verdict.as_str() {
            "agree" => "✓".green(),
            "diverge" => "✗".red(),
            _ => "?".yellow(),
        };
        println!("  {mark} {:<18} {}", c.clause, c.detail.dimmed());
    }
}

// ---------------------------------------------------------------------------
// Session resolution
// ---------------------------------------------------------------------------

fn resolve_session(ctx: &Context, s: &str) -> anyhow::Result<Session> {
    // 1. Try the canonical presence registry.
    let self_state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let presence_path = self_state_dir
        .join("presence")
        .join(s)
        .join("presence.json");
    if presence_path.exists() {
        let bytes = std::fs::read(&presence_path)?;
        let presence: Presence = serde_json::from_slice(&bytes)?;
        return Ok(Session {
            label: s.to_owned(),
            state_dir: presence.cwd.join(".cosmon").join("state"),
            cwd: presence.cwd,
        });
    }

    // 2. Fall back to filesystem-path resolution.
    let p = PathBuf::from(s);
    if p.is_dir() {
        let candidate = p.join(".cosmon").join("state");
        if candidate.is_dir() {
            return Ok(Session {
                label: s.to_owned(),
                state_dir: candidate,
                cwd: p,
            });
        }
        // Caller passed the state dir directly?
        if p.ends_with("state") && p.is_dir() {
            let cwd = p
                .parent()
                .and_then(Path::parent)
                .map_or_else(|| p.clone(), Path::to_path_buf);
            return Ok(Session {
                label: s.to_owned(),
                state_dir: p,
                cwd,
            });
        }
    }

    Err(anyhow::anyhow!(
        "cannot resolve session '{s}': not a known presence id, not a galaxy directory"
    ))
}

fn resolve_molecule_prefix(prefix: &str) -> anyhow::Result<MoleculeId> {
    // Accept full ids as-is; prefixes are resolved per-session against
    // their own state store inside `load_molecule`. The parse here is
    // purely syntactic — we require the full id at this layer to avoid
    // silently picking different molecules on each side.
    MoleculeId::new(prefix).map_err(|e| anyhow::anyhow!("invalid molecule id '{prefix}': {e}"))
}

fn load_molecule(session: &Session, id: &MoleculeId) -> anyhow::Result<MoleculeData> {
    let store = super::open_store(&session.state_dir);
    store
        .load_molecule(id)
        .map_err(|e| anyhow::anyhow!("session {}: {e}", session.label))
}

// ---------------------------------------------------------------------------
// Git probes
// ---------------------------------------------------------------------------

fn git_head(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn git_merge_base(cwd: &Path, a: &str, b: &str) -> Option<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["merge-base", a, b])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{BriefingSeal, MoleculeData, StateStore};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_molecule(id: &str, step: usize, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: step,
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
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    fn mk_session(label: &str) -> (TempDir, Session) {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state).unwrap();
        let s = Session {
            label: label.to_owned(),
            state_dir: state,
            cwd: tmp.path().to_path_buf(),
        };
        (tmp, s)
    }

    fn put_mol(session: &Session, mol: &MoleculeData) {
        let store = FileStore::new(&session.state_dir);
        store.save_molecule(&mol.id, mol).unwrap();
    }

    #[test]
    fn verdict_eq_detects_equality() {
        let v = verdict_eq("state", "running", "running");
        assert_eq!(v.verdict, "agree");
    }

    #[test]
    fn verdict_eq_detects_disagreement() {
        let v = verdict_eq("state", "running", "completed");
        assert_eq!(v.verdict, "diverge");
        assert!(v.detail.contains("running"));
        assert!(v.detail.contains("completed"));
    }

    #[test]
    fn agreeing_sessions_return_agree_on_molecule_clauses() {
        let (_ta, a) = mk_session("A");
        let (_tb, b) = mk_session("B");
        let mol = make_molecule("task-20260424-aaaa", 1, MoleculeStatus::Running);
        put_mol(&a, &mol);
        put_mol(&b, &mol);
        let report = compute_report(&a, &b, Some(&mol.id));
        let state_verdict = report.clauses.iter().find(|c| c.clause == "state").unwrap();
        assert_eq!(state_verdict.verdict, "agree");
        let step_verdict = report
            .clauses
            .iter()
            .find(|c| c.clause == "current_step")
            .unwrap();
        assert_eq!(step_verdict.verdict, "agree");
    }

    #[test]
    fn diverging_state_yields_diverge() {
        let (_ta, a) = mk_session("A");
        let (_tb, b) = mk_session("B");
        let ma = make_molecule("task-20260424-bbbb", 1, MoleculeStatus::Running);
        let mb = make_molecule("task-20260424-bbbb", 1, MoleculeStatus::Completed);
        put_mol(&a, &ma);
        put_mol(&b, &mb);
        let report = compute_report(&a, &b, Some(&ma.id));
        let state_verdict = report.clauses.iter().find(|c| c.clause == "state").unwrap();
        assert_eq!(state_verdict.verdict, "diverge");
        assert!(["diverge", "inconclusive"].contains(&report.outcome.as_str()));
    }

    #[test]
    fn diverging_briefing_seals_detected() {
        let (_ta, a) = mk_session("A");
        let (_tb, b) = mk_session("B");
        let mut ma = make_molecule("task-20260424-cccc", 1, MoleculeStatus::Running);
        let mut mb = ma.clone();
        ma.briefing_seals.push(BriefingSeal::of_bytes(0, b"A"));
        mb.briefing_seals.push(BriefingSeal::of_bytes(0, b"B"));
        put_mol(&a, &ma);
        put_mol(&b, &mb);
        let report = compute_report(&a, &b, Some(&ma.id));
        let seals_verdict = report
            .clauses
            .iter()
            .find(|c| c.clause == "briefing_seals")
            .unwrap();
        assert_eq!(seals_verdict.verdict, "diverge");
    }

    #[test]
    fn missing_molecule_is_inconclusive_not_diverge() {
        let (_ta, a) = mk_session("A");
        let (_tb, b) = mk_session("B");
        let mol = make_molecule("task-20260424-dddd", 1, MoleculeStatus::Running);
        put_mol(&a, &mol);
        // intentionally do not put mol in b
        let report = compute_report(&a, &b, Some(&mol.id));
        let state_verdict = report.clauses.iter().find(|c| c.clause == "state").unwrap();
        assert_eq!(state_verdict.verdict, "inconclusive");
    }

    #[test]
    fn no_molecule_argument_yields_inconclusive_clauses() {
        let (_ta, a) = mk_session("A");
        let (_tb, b) = mk_session("B");
        let report = compute_report(&a, &b, None);
        for clause in ["state", "current_step", "briefing_seals"] {
            let v = report.clauses.iter().find(|c| c.clause == clause).unwrap();
            assert_eq!(v.verdict, "inconclusive");
        }
    }

    #[test]
    fn agreement_exit_codes() {
        assert_eq!(Agreement::Agree.exit_code(), 0);
        assert_eq!(Agreement::Diverge.exit_code(), 1);
        assert_eq!(Agreement::Inconclusive.exit_code(), 2);
    }
}
