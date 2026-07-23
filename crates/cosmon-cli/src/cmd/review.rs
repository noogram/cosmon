// SPDX-License-Identifier: AGPL-3.0-only

//! `cs session review` — verdict-door for router-staged molecules.
//!
//! The single interaction surface through which the operator approves,
//! dismisses, or undoes router proposals. Consumes the staged molecules
//! emitted by `cs session route` (tag `temp:proposed` +
//! `session-note:<sid>@<HH-MM-SS>`) and surfaces them as a markdown
//! review file opened in `$EDITOR`.
//!
//! # Three affordances
//!
//! Per ADR-072 §7: `keep`, `dismiss`, `undo` — nothing else. No fourth
//! button, no rules editor, no threshold slider.
//!
//! | Verdict | Effect |
//! |---------|--------|
//! | `keep` | Drops `temp:proposed`, adds `temp:hot`. Molecule enters normal backlog. |
//! | `dismiss` | `cs collapse <id> --reason router_discarded`. |
//! | `undo` | Hard-deletes the staged molecule directory. The raw note in the session carnet is never touched (invariant I4). |
//!
//! # Silent when empty
//!
//! When no staged molecules are pending review, the verb exits `0` with
//! a *"nothing to review"* message and does **not** open the editor. This
//! is the briefing's unanimous Jobs-panel cut — silence is valid.
//!
//! # Confidence numbers never surface in the UI
//!
//! The sidecars carry per-axis confidences in `[0.0, 1.0]`; the review
//! file translates them into plain labels (`spark candidate`,
//! `question candidate`, `proposed`). Raw numbers live in the sidecars
//! for debugging only.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::tag::Tag;
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};

use super::Context;

/// Staging tag — matches the tag applied by `cs session route`.
const TEMP_PROPOSED: &str = "temp:proposed";

/// Promotion tag applied on `keep`.
const TEMP_HOT: &str = "temp:hot";

/// Collapse reason used on `dismiss` (stable string — operator-facing
/// and scripting-facing so it MUST NOT drift).
const ROUTER_DISCARDED: &str = "router_discarded";

/// Arguments for `cs session review`.
#[derive(clap::Args, Debug)]
pub struct ReviewArgs {
    /// Session id (e.g. `session-2026-04-22T10-31-31Z`). When omitted,
    /// review every session that has pending staged molecules.
    pub session: Option<String>,

    /// Parse the previously-composed review file and apply the
    /// verdicts. Without this flag, the verb composes the markdown
    /// review file and opens it in `$EDITOR`.
    #[arg(long)]
    pub apply: bool,

    /// Override the editor (defaults to `$EDITOR`, then `vi`). Ignored
    /// with `--apply` and with `--json`.
    #[arg(long, value_name = "CMD")]
    pub editor: Option<String>,
}

/// Entry point for `cs session review`.
///
/// # Errors
/// Propagates filestore, I/O, editor-spawn, and verdict-parse errors.
pub fn run(ctx: &Context, args: &ReviewArgs) -> anyhow::Result<()> {
    let state_dir = ctx.state_dir();
    let store = ctx.store();

    let proposals = collect_proposals(store.as_ref(), args.session.as_deref())?;

    if proposals.is_empty() {
        emit_nothing_to_review(ctx, args.session.as_deref());
        return Ok(());
    }

    let review_dir = state_dir.join("sessions").join(".review");
    let review_path = review_file_path(&review_dir, args.session.as_deref());

    if args.apply {
        return apply_verdicts(ctx, store.as_ref(), &proposals, &review_path);
    }

    if ctx.json {
        emit_proposals_ndjson(&proposals, &review_path);
        return Ok(());
    }

    fs::create_dir_all(&review_dir)
        .map_err(|e| anyhow::anyhow!("mkdir {}: {e}", review_dir.display()))?;
    let body = render_review_markdown(&proposals);
    fs::write(&review_path, body)
        .map_err(|e| anyhow::anyhow!("write review {}: {e}", review_path.display()))?;

    open_editor(&review_path, args.editor.as_deref())?;

    println!(
        "review file: {}\nrun `cs session review{} --apply` when ready.",
        review_path.display(),
        args.session
            .as_deref()
            .map(|s| format!(" {s}"))
            .unwrap_or_default(),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Proposal collection
// ---------------------------------------------------------------------------

/// One pending proposal — a staged molecule that has not yet been
/// reviewed.
#[derive(Clone, Debug)]
pub struct Proposal {
    /// Molecule ID (e.g. `spark-20260424-abcd`).
    pub mol_id: MoleculeId,
    /// Session id decoded from the `session-note:` tag.
    pub session_id: String,
    /// `HH:MM:SS` note timestamp decoded from the `session-note:` tag.
    pub note_ts: String,
    /// Note body (taken from the molecule's `topic` variable).
    pub body: String,
    /// Proposed action label for the UI — one of `spark candidate`,
    /// `question candidate`, `idea candidate`, `chronicle candidate`,
    /// `proposed`.
    pub label: String,
    /// Note tag (tag field on the session note, e.g. `insight`). Empty
    /// when absent.
    pub note_tag: String,
}

/// Walk every `temp:proposed` molecule and narrow to those tagged
/// `session-note:<sid>@*`. If `session` is `None`, every session is
/// included.
fn collect_proposals(
    store: &dyn StateStore,
    session: Option<&str>,
) -> anyhow::Result<Vec<Proposal>> {
    let filter = MoleculeFilter {
        tag_globs: vec![TEMP_PROPOSED.to_owned()],
        ..Default::default()
    };
    let molecules = store
        .list_molecules(&filter)
        .map_err(|e| anyhow::anyhow!("list_molecules: {e}"))?;

    let mut out = Vec::new();
    for mol in molecules {
        // Terminal molecules keep the `temp:proposed` tag for provenance
        // but have nothing left to review — skip them so the operator
        // does not re-see a prior dismissal or completion.
        if matches!(
            mol.status,
            MoleculeStatus::Collapsed | MoleculeStatus::Completed
        ) {
            continue;
        }
        let Some((sid, ts)) = extract_session_note(&mol) else {
            continue;
        };
        if let Some(want) = session {
            if sid != want {
                continue;
            }
        }
        out.push(Proposal {
            mol_id: mol.id.clone(),
            session_id: sid,
            note_ts: ts,
            body: mol.variables.get("topic").cloned().unwrap_or_default(),
            label: label_for(&mol).to_owned(),
            note_tag: mol.variables.get("note_tag").cloned().unwrap_or_default(),
        });
    }

    // Deterministic order for stable review files: by session id, then
    // timestamp, then molecule id (tie-breaker on duplicates).
    out.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then_with(|| a.note_ts.cmp(&b.note_ts))
            .then_with(|| a.mol_id.as_str().cmp(b.mol_id.as_str()))
    });
    Ok(out)
}

/// Pull the `session-note:<sid>@<HH-MM-SS>` tag off a molecule and
/// return `(sid, HH:MM:SS)`. Returns `None` when the tag is missing —
/// the caller filters out non-session proposals.
fn extract_session_note(mol: &MoleculeData) -> Option<(String, String)> {
    for tag in &mol.tags {
        let raw = tag.as_str();
        let Some(rest) = raw.strip_prefix("session-note:") else {
            continue;
        };
        let (sid, ts_tag) = rest.split_once('@')?;
        // The route tag encodes `:` as `-` (filesystem-safe); translate
        // back to canonical `HH:MM:SS`.
        let ts = ts_tag_to_hhmmss(ts_tag)?;
        return Some((sid.to_owned(), ts));
    }
    None
}

/// Translate a route-encoded `HH-MM-SS` tag fragment back to
/// `HH:MM:SS`. Returns `None` on malformed inputs.
fn ts_tag_to_hhmmss(ts_tag: &str) -> Option<String> {
    let b = ts_tag.as_bytes();
    if b.len() != 8 || b[2] != b'-' || b[5] != b'-' {
        return None;
    }
    if !b[0].is_ascii_digit()
        || !b[1].is_ascii_digit()
        || !b[3].is_ascii_digit()
        || !b[4].is_ascii_digit()
        || !b[6].is_ascii_digit()
        || !b[7].is_ascii_digit()
    {
        return None;
    }
    Some(format!(
        "{}:{}:{}",
        &ts_tag[0..2],
        &ts_tag[3..5],
        &ts_tag[6..8]
    ))
}

/// Translate a molecule's shape into a plain UI label. We never surface
/// the raw confidence numbers per the briefing §UX discipline.
fn label_for(mol: &MoleculeData) -> &'static str {
    // The staging formula is stamped in `formula_id`. Today only the
    // `spark` formula reaches review; future `idea`/`chronicle` formulas
    // will fan out here without changing the contract.
    match mol.formula_id.as_str() {
        "spark" => "spark candidate",
        "idea" => "idea candidate",
        _ => "proposed",
    }
}

// ---------------------------------------------------------------------------
// Markdown rendering + parsing
// ---------------------------------------------------------------------------

/// Marker on every verdict line so the parser finds them reliably. The
/// operator fills the blank after `verdict:`.
const VERDICT_MARKER: &str = "verdict:";

/// Render the review file text. One section per proposal with a blank
/// verdict line. The molecule id is embedded as an HTML comment so the
/// parser can find it even if the operator reorders sections.
fn render_review_markdown(proposals: &[Proposal]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("# Session review\n\n");
    out.push_str(
        "Write `keep`, `dismiss`, or `undo` on each `verdict:` line,\n\
         save, then run `cs session review --apply`.\n\n\
         - `keep`    → promotes the molecule to `temp:hot` (normal backlog).\n\
         - `dismiss` → `cs collapse` with reason `router_discarded`.\n\
         - `undo`    → hard-deletes the staged molecule (the raw note\n  is untouched — the carnet stays sealed).\n\n",
    );

    let mut current_sid: Option<&str> = None;
    for p in proposals {
        if current_sid != Some(p.session_id.as_str()) {
            let _ = writeln!(out, "## {}\n", p.session_id);
            current_sid = Some(p.session_id.as_str());
        }
        let _ = writeln!(out, "### {} — [{}]", p.note_ts, p.label);
        let _ = writeln!(out, "<!-- mol: {} -->", p.mol_id);
        if !p.note_tag.is_empty() {
            let _ = writeln!(out, "_tag: {}_", p.note_tag);
        }
        out.push('\n');
        // Quote every body line so the markdown renders as a blockquote
        // and the parser cannot confuse body text with a verdict line.
        for line in p.body.lines() {
            let _ = writeln!(out, "> {line}");
        }
        if p.body.lines().next().is_none() {
            out.push_str("> (empty body)\n");
        }
        out.push('\n');
        out.push_str(VERDICT_MARKER);
        out.push_str(" ____   (keep / dismiss / undo)\n\n");
    }
    out
}

/// One parsed verdict: `(molecule_id, verdict_text)`.
#[derive(Debug)]
struct ParsedVerdict {
    mol_id: String,
    verdict: Verdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Keep,
    Dismiss,
    Undo,
}

impl Verdict {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "keep" | "k" => Some(Self::Keep),
            "dismiss" | "d" => Some(Self::Dismiss),
            "undo" | "u" => Some(Self::Undo),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Dismiss => "dismiss",
            Self::Undo => "undo",
        }
    }
}

/// Parse a previously-rendered review file. Each section begins with a
/// `<!-- mol: <id> -->` comment and ends with a `verdict:` line whose
/// value is `keep` / `dismiss` / `undo` (case-insensitive). Blank
/// verdicts — the default from [`render_review_markdown`] — are
/// skipped (no-op) rather than errored so the operator can leave
/// sections un-reviewed and come back.
fn parse_review_markdown(content: &str) -> Vec<ParsedVerdict> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("<!-- mol:") {
            let mol = rest.trim_end_matches("-->").trim();
            if !mol.is_empty() {
                current = Some(mol.to_owned());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(VERDICT_MARKER) {
            let v = rest
                .split_once('(')
                .map_or(rest, |(lhs, _)| lhs)
                .trim()
                .trim_matches('_')
                .trim();
            if let (Some(mol), Some(verdict)) = (current.take(), Verdict::parse(v)) {
                out.push(ParsedVerdict {
                    mol_id: mol,
                    verdict,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Apply path
// ---------------------------------------------------------------------------

fn apply_verdicts(
    ctx: &Context,
    store: &dyn StateStore,
    proposals: &[Proposal],
    review_path: &Path,
) -> anyhow::Result<()> {
    if !review_path.is_file() {
        anyhow::bail!(
            "review file not found at {} — run `cs session review` first to compose it",
            review_path.display()
        );
    }
    let content = fs::read_to_string(review_path)
        .map_err(|e| anyhow::anyhow!("read review {}: {e}", review_path.display()))?;
    let parsed = parse_review_markdown(&content);

    let proposal_by_id: BTreeMap<&str, &Proposal> =
        proposals.iter().map(|p| (p.mol_id.as_str(), p)).collect();

    let mut kept = 0usize;
    let mut dismissed = 0usize;
    let mut undone = 0usize;
    let mut errors = 0usize;

    for v in &parsed {
        let Some(p) = proposal_by_id.get(v.mol_id.as_str()) else {
            // Verdict on a molecule that no longer has `temp:proposed` —
            // likely reviewed from a previous pass. Silent skip.
            continue;
        };
        let outcome = match v.verdict {
            Verdict::Keep => apply_keep(store, &p.mol_id),
            Verdict::Dismiss => apply_dismiss(store, &p.mol_id),
            Verdict::Undo => apply_undo(store, &p.mol_id),
        };
        match &outcome {
            Ok(()) => match v.verdict {
                Verdict::Keep => kept += 1,
                Verdict::Dismiss => dismissed += 1,
                Verdict::Undo => undone += 1,
            },
            Err(e) => {
                errors += 1;
                eprintln!("verdict {} on {} failed: {e}", v.verdict.as_str(), v.mol_id);
            }
        }
        emit_verdict_event(ctx, v, p, &outcome);
    }

    if ctx.json {
        let summary = serde_json::json!({
            "event": "review_apply_complete",
            "kept": kept,
            "dismissed": dismissed,
            "undone": undone,
            "errors": errors,
            "total_verdicts": parsed.len(),
            "pending_proposals": proposals.len(),
        });
        println!("{summary}");
    } else {
        println!(
            "review applied: {kept} kept, {dismissed} dismissed, {undone} undone, {errors} errors \
             ({} verdicts, {} proposals)",
            parsed.len(),
            proposals.len(),
        );
    }

    if errors > 0 {
        anyhow::bail!("{errors} verdict(s) failed — see stderr for details");
    }
    Ok(())
}

/// `keep`: drop `temp:proposed`, add `temp:hot`. Idempotent — a
/// molecule already in `temp:hot` simply loses the proposed tag.
fn apply_keep(store: &dyn StateStore, mol_id: &MoleculeId) -> anyhow::Result<()> {
    let mut mol = store
        .load_molecule(mol_id)
        .map_err(|e| anyhow::anyhow!("load {mol_id}: {e}"))?;
    let proposed = Tag::new(TEMP_PROPOSED.to_owned())
        .map_err(|e| anyhow::anyhow!("invalid tag `{TEMP_PROPOSED}`: {e}"))?;
    let hot = Tag::new(TEMP_HOT.to_owned())
        .map_err(|e| anyhow::anyhow!("invalid tag `{TEMP_HOT}`: {e}"))?;
    mol.tags.remove(&proposed);
    mol.tags.insert(hot);
    mol.updated_at = chrono::Utc::now();
    store
        .save_molecule(&mol.id.clone(), &mol)
        .map_err(|e| anyhow::anyhow!("save {mol_id}: {e}"))?;
    Ok(())
}

/// `dismiss`: inline collapse — flip the molecule to `Collapsed` with
/// reason `router_discarded`. We mirror `cs collapse`'s core state
/// transition (status + reason + `updated_at`) without shelling out: the
/// review loop runs over dozens of proposals at a time, and a fork per
/// row is both slow and fragile (the child `cs` needs to inherit the
/// same config context). See `cmd/collapse.rs` for the external verb;
/// both paths land in the same state store.
fn apply_dismiss(store: &dyn StateStore, mol_id: &MoleculeId) -> anyhow::Result<()> {
    let mut mol = store
        .load_molecule(mol_id)
        .map_err(|e| anyhow::anyhow!("load {mol_id}: {e}"))?;
    if mol.status == MoleculeStatus::Collapsed {
        return Ok(());
    }
    mol.status = MoleculeStatus::Collapsed;
    mol.collapse_reason = Some(ROUTER_DISCARDED.to_owned());
    mol.collapsed_step = Some(mol.current_step);
    mol.updated_at = chrono::Utc::now();
    store
        .save_molecule(&mol.id.clone(), &mol)
        .map_err(|e| anyhow::anyhow!("save {mol_id}: {e}"))?;
    Ok(())
}

/// `undo`: hard-delete the staged molecule directory. Guard-railed so
/// only a `temp:proposed` molecule is ever removed — otherwise the
/// operator could nuke a real backlog entry through a stale review
/// file.
fn apply_undo(store: &dyn StateStore, mol_id: &MoleculeId) -> anyhow::Result<()> {
    let mol = store
        .load_molecule(mol_id)
        .map_err(|e| anyhow::anyhow!("load {mol_id}: {e}"))?;
    let proposed = Tag::new(TEMP_PROPOSED.to_owned())
        .map_err(|e| anyhow::anyhow!("invalid tag `{TEMP_PROPOSED}`: {e}"))?;
    if !mol.tags.contains(&proposed) {
        anyhow::bail!(
            "refusing to undo {mol_id}: not tagged {TEMP_PROPOSED} — \
             the molecule may have left the review queue since this file was composed"
        );
    }
    let dir = store.molecule_dir(&mol.id);
    fs::remove_dir_all(&dir).map_err(|e| anyhow::anyhow!("remove {}: {e}", dir.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Editor + filesystem helpers
// ---------------------------------------------------------------------------

fn open_editor(path: &Path, override_cmd: Option<&str>) -> anyhow::Result<()> {
    let editor = override_cmd
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("VISUAL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".to_owned());

    // Resolve editor argv — honour `EDITOR="code -w"` style commands.
    let mut parts = editor.split_whitespace();
    let Some(bin) = parts.next() else {
        anyhow::bail!("empty editor command");
    };
    let rest: Vec<&str> = parts.collect();

    let status = Command::new(bin)
        .args(rest)
        .arg(path)
        .status()
        .map_err(|e| anyhow::anyhow!("spawn {bin}: {e}"))?;
    if !status.success() {
        anyhow::bail!("editor {bin} exited with {status}");
    }
    Ok(())
}

fn review_file_path(review_dir: &Path, session: Option<&str>) -> PathBuf {
    match session {
        Some(s) => review_dir.join(format!("{s}.md")),
        None => review_dir.join("pending.md"),
    }
}

fn emit_nothing_to_review(ctx: &Context, session: Option<&str>) {
    if ctx.json {
        let out = serde_json::json!({
            "event": "nothing_to_review",
            "session": session,
        });
        println!("{out}");
    } else {
        match session {
            Some(s) => println!("nothing to review for session {s} — no staged proposals."),
            None => println!("nothing to review — no staged proposals."),
        }
    }
}

fn emit_proposals_ndjson(proposals: &[Proposal], review_path: &Path) {
    for p in proposals {
        let row = serde_json::json!({
            "event": "proposal_pending",
            "molecule_id": p.mol_id.as_str(),
            "session_id": p.session_id,
            "note_ts": p.note_ts,
            "label": p.label,
            "note_tag": p.note_tag,
            "body": p.body,
            "review_file": review_path.display().to_string(),
        });
        println!("{row}");
    }
    let summary = serde_json::json!({
        "event": "review_compose_complete",
        "proposals": proposals.len(),
        "review_file": review_path.display().to_string(),
    });
    println!("{summary}");
}

fn emit_verdict_event(
    ctx: &Context,
    verdict: &ParsedVerdict,
    proposal: &Proposal,
    outcome: &anyhow::Result<()>,
) {
    if !ctx.json {
        return;
    }
    let (ok, err) = match outcome {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    let row = serde_json::json!({
        "event": "verdict_applied",
        "molecule_id": verdict.mol_id,
        "session_id": proposal.session_id,
        "note_ts": proposal.note_ts,
        "verdict": verdict.verdict.as_str(),
        "ok": ok,
        "error": err,
    });
    println!("{row}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::tag::Tag;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, StateStore};

    use super::*;

    fn mol(id: &str, formula: &str, topic: &str, tags: &[&str]) -> MoleculeData {
        let now = Utc::now();
        let mut vars = HashMap::new();
        vars.insert("topic".to_owned(), topic.to_owned());
        let tag_set: BTreeSet<Tag> = tags
            .iter()
            .map(|t| Tag::new((*t).to_owned()).expect("valid tag in test fixture"))
            .collect();
        MoleculeData {
            id: MoleculeId::new(id).expect("valid id"),
            fleet_id: FleetId::new("default").expect("valid fleet"),
            formula_id: FormulaId::new(formula).expect("valid formula"),
            status: MoleculeStatus::Pending,
            variables: vars,
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 1,
            current_step: 0,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: vec![],
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![],
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: tag_set,
            escalations: vec![],
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
            adapter: None,
        }
    }

    #[test]
    fn ts_tag_roundtrip() {
        assert_eq!(ts_tag_to_hhmmss("10-31-15"), Some("10:31:15".to_owned()));
        assert_eq!(ts_tag_to_hhmmss("10:31:15"), None);
        assert_eq!(ts_tag_to_hhmmss("bad"), None);
    }

    #[test]
    fn extract_session_note_from_tag() {
        let m = mol(
            "spark-20260424-1111",
            "spark",
            "hello",
            &[
                "temp:proposed",
                "session-note:session-2026-04-22T10-31-31Z@10-32-05",
            ],
        );
        let got = extract_session_note(&m).expect("tag present");
        assert_eq!(got.0, "session-2026-04-22T10-31-31Z");
        assert_eq!(got.1, "10:32:05");
    }

    #[test]
    fn render_round_trips_through_parser() {
        let proposals = vec![
            Proposal {
                mol_id: MoleculeId::new("spark-20260424-1111").unwrap(),
                session_id: "session-x".to_owned(),
                note_ts: "10:00:00".to_owned(),
                body: "first\nwith two lines".to_owned(),
                label: "spark candidate".to_owned(),
                note_tag: "insight".to_owned(),
            },
            Proposal {
                mol_id: MoleculeId::new("spark-20260424-2222").unwrap(),
                session_id: "session-x".to_owned(),
                note_ts: "10:05:00".to_owned(),
                body: "second".to_owned(),
                label: "question candidate".to_owned(),
                note_tag: String::new(),
            },
        ];
        let rendered = render_review_markdown(&proposals);
        // Blank verdicts do not produce rows.
        assert!(parse_review_markdown(&rendered).is_empty());

        // Operator fills in verdicts.
        let filled = rendered
            .replace(
                "verdict: ____   (keep / dismiss / undo)\n\n### 10:05:00",
                "verdict: keep\n\n### 10:05:00",
            )
            .replace(
                "verdict: ____   (keep / dismiss / undo)\n",
                "verdict: dismiss\n",
            );
        let parsed = parse_review_markdown(&filled);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].mol_id, "spark-20260424-1111");
        assert_eq!(parsed[0].verdict, Verdict::Keep);
        assert_eq!(parsed[1].mol_id, "spark-20260424-2222");
        assert_eq!(parsed[1].verdict, Verdict::Dismiss);
    }

    #[test]
    fn parser_ignores_non_verdict_lines() {
        let content =
            "# header\n\n<!-- mol: spark-x -->\n\n> verdict: keep (in a quote)\n\nverdict: keep\n";
        let got = parse_review_markdown(content);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].verdict, Verdict::Keep);
    }

    #[test]
    fn parser_tolerates_shorthand() {
        let content = "<!-- mol: a -->\nverdict: k\n<!-- mol: b -->\nverdict: d\n<!-- mol: c -->\nverdict: u\n";
        let got = parse_review_markdown(content);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].verdict, Verdict::Keep);
        assert_eq!(got[1].verdict, Verdict::Dismiss);
        assert_eq!(got[2].verdict, Verdict::Undo);
    }

    #[test]
    fn collect_proposals_filters_by_session() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);

        let a = mol(
            "spark-20260424-aaaa",
            "spark",
            "body a",
            &["temp:proposed", "session-note:session-A@10-00-00"],
        );
        let b = mol(
            "spark-20260424-bbbb",
            "spark",
            "body b",
            &["temp:proposed", "session-note:session-B@10-00-00"],
        );
        // A molecule without the staging tag — must be excluded.
        let c = mol(
            "spark-20260424-cccc",
            "spark",
            "body c",
            &["temp:hot", "session-note:session-A@11-00-00"],
        );
        store.save_molecule(&a.id, &a).unwrap();
        store.save_molecule(&b.id, &b).unwrap();
        store.save_molecule(&c.id, &c).unwrap();

        let all = collect_proposals(&store, None).unwrap();
        assert_eq!(all.len(), 2);

        let only_a = collect_proposals(&store, Some("session-A")).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].mol_id.as_str(), "spark-20260424-aaaa");
    }

    #[test]
    fn collect_proposals_skips_terminal_molecules() {
        // A previously-dismissed molecule keeps `temp:proposed` on disk
        // for provenance, but must not re-surface in the review queue.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);

        let mut collapsed = mol(
            "spark-20260424-cold",
            "spark",
            "collapsed body",
            &["temp:proposed", "session-note:session-X@09-00-00"],
        );
        collapsed.status = MoleculeStatus::Collapsed;
        collapsed.collapse_reason = Some("router_discarded".to_owned());
        store.save_molecule(&collapsed.id, &collapsed).unwrap();

        let mut completed = mol(
            "spark-20260424-warm",
            "spark",
            "completed body",
            &["temp:proposed", "session-note:session-X@09-05-00"],
        );
        completed.status = MoleculeStatus::Completed;
        store.save_molecule(&completed.id, &completed).unwrap();

        let pending = mol(
            "spark-20260424-liv1",
            "spark",
            "still pending",
            &["temp:proposed", "session-note:session-X@09-10-00"],
        );
        store.save_molecule(&pending.id, &pending).unwrap();

        let got = collect_proposals(&store, None).unwrap();
        assert_eq!(got.len(), 1, "only the pending proposal should surface");
        assert_eq!(got[0].mol_id.as_str(), "spark-20260424-liv1");
    }

    #[test]
    fn apply_keep_swaps_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);

        let m = mol(
            "spark-20260424-keep",
            "spark",
            "body",
            &["temp:proposed", "session-note:session-A@10-00-00"],
        );
        store.save_molecule(&m.id, &m).unwrap();

        apply_keep(&store, &m.id).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        let proposed = Tag::new("temp:proposed".to_owned()).unwrap();
        let hot = Tag::new("temp:hot".to_owned()).unwrap();
        assert!(!reloaded.tags.contains(&proposed));
        assert!(reloaded.tags.contains(&hot));
    }

    #[test]
    fn apply_undo_refuses_untagged() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);

        // Not tagged temp:proposed — undo must refuse.
        let m = mol(
            "spark-20260424-un01",
            "spark",
            "body",
            &["temp:hot", "session-note:session-A@10-00-00"],
        );
        store.save_molecule(&m.id, &m).unwrap();

        let err = apply_undo(&store, &m.id).unwrap_err();
        assert!(
            err.to_string().contains("refusing to undo"),
            "unexpected error: {err}"
        );

        // Directory must still exist — no destructive effect.
        let dir = store.molecule_dir(&m.id);
        assert!(dir.is_dir(), "directory should survive refused undo");
    }

    #[test]
    fn apply_undo_removes_dir_when_proposed() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);

        let m = mol(
            "spark-20260424-un02",
            "spark",
            "body",
            &["temp:proposed", "session-note:session-A@10-00-00"],
        );
        store.save_molecule(&m.id, &m).unwrap();
        let dir = store.molecule_dir(&m.id);
        assert!(dir.is_dir());

        apply_undo(&store, &m.id).unwrap();
        assert!(!dir.exists(), "directory should be gone after undo");
    }

    #[test]
    fn review_markdown_renders_stable_sections() {
        // The render is sorted by (session, ts, id) so the file is
        // byte-stable across re-runs as long as the proposal set is
        // unchanged.
        let proposals = vec![
            Proposal {
                mol_id: MoleculeId::new("spark-20260424-sp02").unwrap(),
                session_id: "session-A".to_owned(),
                note_ts: "10:05:00".to_owned(),
                body: "b".to_owned(),
                label: "spark candidate".to_owned(),
                note_tag: String::new(),
            },
            Proposal {
                mol_id: MoleculeId::new("spark-20260424-sp01").unwrap(),
                session_id: "session-A".to_owned(),
                note_ts: "10:00:00".to_owned(),
                body: "a".to_owned(),
                label: "spark candidate".to_owned(),
                note_tag: String::new(),
            },
        ];
        let rendered = render_review_markdown(&proposals);
        let pos_1 = rendered
            .find("spark-20260424-sp01")
            .expect("spark-1 present");
        let pos_2 = rendered
            .find("spark-20260424-sp02")
            .expect("spark-2 present");
        // Unsorted input — the renderer preserves caller order, so
        // spark-2 appears first here. The sort is applied at
        // [`collect_proposals`] before this function sees the slice.
        assert!(pos_2 < pos_1);
    }
}
