// SPDX-License-Identifier: AGPL-3.0-only

//! `cs session route` — Tier-1 regex classifier + sidecar writer (ADR-072).
//!
//! Walks `.cosmon/state/sessions/session-*.md`, extracts each note body,
//! applies the Tier-1 regex cascade, and writes one
//! `.cosmon/state/sessions/.route/<sid>/<body_hash>.json` sidecar per
//! note. When the classifier returns `max(confidence) ≥ staging_threshold`
//! the handler nucleates a `temp:proposed` molecule via `cs nucleate`.
//!
//! This is the v0 implementation of ADR-072 — tier 1 only. Tiers 2–4
//! (local LLM, cloud LLM, human verdict-door) are separate follow-ups.
//! Low-confidence notes are written as `tier4_pending` sidecars;
//! writing `axes = null` is the transient escalation marker per
//! amendment A1 (no terminal orphans at tier 1).
//!
//! The shell `scripts/session-route-tick.sh` is a thin `LaunchAgent`
//! wrapper that simply invokes `cs session route --all --json`; all
//! classification logic lives here so it is type-checked, unit-tested,
//! and reuses [`cosmon_hash`] for the BLAKE3 `body_hash`.
//!
//! # Invariants (Shannon, verbatim from ADR-072 §Formal invariants)
//!
//! - **I1 body-primacy**: every sidecar carries `body_hash = BLAKE3(body)`.
//! - **I2 idempotent pure**: same `(body, router_version, prompt_version)`
//!   → byte-identical sidecar on re-run.
//! - **I3 append-only**: bumping `router_version` writes a **new** sidecar
//!   file (different `body_hash` → different path; same body + new
//!   router version → new file named `<body_hash>.<router_version>.json`).
//! - **I4 carnet untouched**: the session `*.md` file is never opened
//!   for write by this handler.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{SecondsFormat, Utc};
use cosmon_hash::Hash;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::Context;

/// Router identity string. Bumping this triggers append-only sidecar
/// writes (invariant I3) — never overwrite.
pub const ROUTER_VERSION: &str = "route-v1-2026-04-24";

/// Tier-1 prompt identity. Tier 1 has no prompt *per se* but the field
/// exists so tier-2/3 evolutions share the schema.
pub const PROMPT_VERSION: &str = "tier1-regex-v1";

/// Staging threshold — below this, a note does not auto-stage a
/// molecule. Default 0.75 per ADR-072 §5 placeholder. A benchmark
/// follow-up will calibrate it.
pub const STAGING_THRESHOLD: f32 = 0.75;

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

/// Arguments for `cs session route`.
#[derive(clap::Args, Debug)]
pub struct RouteArgs {
    /// Session file stem (e.g. `session-2026-04-22T10-31-31Z`) or an
    /// absolute path. When omitted, defaults to scanning every session
    /// file if `--all` is passed, or the currently-open session
    /// otherwise.
    pub session: Option<String>,

    /// Process every session file under `.cosmon/state/sessions/`.
    #[arg(long)]
    pub all: bool,

    /// Print what would be classified without writing sidecars or
    /// nucleating molecules.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Skip auto-nucleation of high-confidence `temp:proposed`
    /// molecules — write sidecars only. Useful when backfilling or
    /// debugging.
    #[arg(long = "no-stage")]
    pub no_stage: bool,

    /// Cap sidecars emitted this run (safety net for batch backfills).
    #[arg(long, default_value_t = 500)]
    pub max: usize,
}

// ---------------------------------------------------------------------------
// Axis vector schema (ADR-072 §4)
// ---------------------------------------------------------------------------

/// Three-axis vector per ADR-072 §4. `⊥` is represented by `None`
/// (JSON `null`) and is only legal in a `tier4_pending` sidecar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Salience {
    /// Attend right now.
    Hot,
    /// Worth keeping warm.
    Warm,
    /// Filed, not urgent.
    Cold,
}

/// Whom the operator is addressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Addressee {
    /// The operator's own future self.
    Self_,
    /// The cosmon system (ask / command / query).
    System,
    /// An external audience (talk, narrative, chronicle).
    Audience,
    /// Another operator or agent.
    Other,
}

/// What kind of downstream move the note calls for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actionability {
    /// A task or todo — an action to take.
    Task,
    /// An idea — a concept to develop.
    Idea,
    /// A reflection — introspective, chronicle-worthy.
    Reflection,
    /// A narrative — editorial, story, dystopia.
    Narrative,
}

/// Full axis triple. All three axes are present in a tier-1/2/3
/// terminal sidecar. A `None` on any field means the tier escalated
/// and the caller should have written `axes: null` with
/// `decided_by: tier4_pending`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Axes {
    /// How urgent.
    pub salience: Salience,
    /// Whom the note speaks to.
    pub addressee: Addressee,
    /// What kind of move.
    pub actionability: Actionability,
}

/// Per-axis confidence in `[0.0, 1.0]`. Never surfaced as a number in
/// UI — translated to an affordance (C5 from the synthesis).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Confidences {
    /// Confidence on the salience axis.
    pub salience: f32,
    /// Confidence on the addressee axis.
    pub addressee: f32,
    /// Confidence on the actionability axis.
    pub actionability: f32,
}

impl Confidences {
    /// The decision-gate value per ADR-072 §5: the *weakest* axis
    /// determines whether we escalate. One low-confidence axis is
    /// enough to send the note up a tier.
    #[must_use]
    pub fn min(self) -> f32 {
        self.salience.min(self.addressee.min(self.actionability))
    }

    /// Maximum across axes — used for status displays.
    #[must_use]
    pub fn max(self) -> f32 {
        self.salience.max(self.addressee.max(self.actionability))
    }
}

/// Rendering hint per ADR-072 §4 — what the UI should propose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposedAction {
    /// Stage a `spark` molecule.
    NucleateSpark,
    /// Stage an `idea` molecule.
    NucleateIdea,
    /// Stage a `spark` tagged as a question awaiting reply.
    NucleateQuestion,
    /// Append to a chronicle draft.
    AppendChronicle,
    /// Tier-4 pending — show in verdict-door as *needs your eye*.
    NeedsYourEye,
    /// Filed, not staged — `cold` salience or reflexive narrative.
    Skip,
}

impl ProposedAction {
    /// Which molecule kind a `keep` verdict on this action should
    /// nucleate. `None` means "never auto-stage" (tier-4, skip).
    ///
    /// `NucleateIdea` currently routes through the `spark` formula
    /// too — there is no distinct `idea` capture formula yet. When
    /// one lands, this arm fans out.
    #[must_use]
    pub fn nucleates_formula(self) -> Option<&'static str> {
        match self {
            Self::NucleateSpark | Self::NucleateQuestion | Self::NucleateIdea => Some("spark"),
            Self::AppendChronicle | Self::NeedsYourEye | Self::Skip => None,
        }
    }
}

/// Map an axis triple to a proposed-action rendering hint
/// (ADR-072 §4 table). Order matters — first match wins.
///
/// Narrative and cold notes both collapse to `Skip` at the rendering
/// layer (same terminal effect: filed, not staged). Audience-directed
/// notes and self-directed tasks both collapse to `NucleateSpark`
/// (the audience tag is applied elsewhere via the `source:session`
/// provenance chain).
#[must_use]
#[allow(clippy::match_same_arms)]
pub fn proposed_action_for(axes: Axes) -> ProposedAction {
    match (axes.salience, axes.addressee, axes.actionability) {
        (_, _, Actionability::Reflection) => ProposedAction::AppendChronicle,
        (_, _, Actionability::Narrative) => ProposedAction::Skip,
        (Salience::Cold, _, _) => ProposedAction::Skip,
        (_, Addressee::System, _) => ProposedAction::NucleateQuestion,
        (_, Addressee::Audience, _) => ProposedAction::NucleateSpark,
        (_, _, Actionability::Task) => ProposedAction::NucleateSpark,
        (_, _, Actionability::Idea) => ProposedAction::NucleateIdea,
    }
}

// ---------------------------------------------------------------------------
// Sidecar on-disk schema
// ---------------------------------------------------------------------------

/// The full sidecar payload per ADR-072 §3.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sidecar {
    /// Human-readable note handle (`<sid>@<HH-MM-SS>`).
    pub note_id: String,
    /// `blake3:<hex>` primary key.
    pub body_hash: String,
    /// Router identity — see [`ROUTER_VERSION`].
    pub router_version: String,
    /// Prompt identity — see [`PROMPT_VERSION`].
    pub prompt_version: String,
    /// The 3-axis triple. `None` only for `tier4_pending` sidecars.
    pub axes: Option<Axes>,
    /// Per-axis confidence. Always written (even for tier-4 pending
    /// so downstream can see why we escalated).
    pub confidences: Confidences,
    /// Rendering hint — the UI shows this, never the axes directly.
    pub proposed_action: ProposedAction,
    /// Which tier produced this sidecar.
    pub decided_by: DecidedBy,
    /// UTC ISO-8601.
    pub decided_at: String,
}

/// Source of the decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    /// Tier 1 — regex (this module).
    Tier1,
    /// Tier 2 — local LLM.
    Tier2Local,
    /// Tier 3 — cloud LLM.
    Tier3Cloud,
    /// Tier 4 — awaiting operator verdict.
    Tier4Pending,
    /// Tier 4 — operator decided.
    Tier4Resolved,
}

// ---------------------------------------------------------------------------
// Tier-1 classifier — the regex cascade
// ---------------------------------------------------------------------------

/// Outcome of one classifier call.
#[derive(Clone, Debug)]
pub struct ClassifierOutcome {
    /// The axes decided (`None` means the classifier escalates).
    pub axes: Option<Axes>,
    /// Confidences per axis. Always populated.
    pub confidences: Confidences,
    /// True when `confidences.min() >= STAGING_THRESHOLD`.
    pub terminal: bool,
}

/// Run the Tier-1 regex cascade over a raw note body.
///
/// Returns axes + confidences + `terminal` flag. The rules are the
/// seed vocabulary from the parent task briefing plus a small set of
/// French markers the operator uses in the 11-note corpus. Order
/// matters: earlier rules override later ones.
///
/// The design intent is Shannon-honest: we only label what the body
/// makes obvious. Ambiguous notes — three unrelated ideas glued
/// together, or a declarative narrative — fall through to the
/// `terminal = false` path and the caller writes a `tier4_pending`
/// sidecar.
#[must_use]
pub fn tier1_classify(body: &str) -> ClassifierOutcome {
    let trimmed = body.trim();
    let first_line = trimmed.lines().next().unwrap_or("").trim();

    // 1. `!spark ` prefix — honest plumbing from the prior regime.
    if let Some(stripped) = first_line.strip_prefix("!spark") {
        let ok = stripped.starts_with(' ') || stripped.is_empty();
        if ok {
            return terminal(Salience::Hot, Addressee::Self_, Actionability::Task, 1.0);
        }
    }

    // 2. TODO/todo prefix — explicit task marker.
    if todo_prefix_re().is_match(first_line) {
        return terminal(Salience::Hot, Addressee::Self_, Actionability::Task, 0.95);
    }

    // 3. "se renseigner" — the operator's recurring "look it up
    //    later" marker. Warm spark. Checked before generic action
    //    verbs because "se" is a non-verb prefix.
    if se_renseigner_re().is_match(first_line) {
        return terminal(Salience::Warm, Addressee::Self_, Actionability::Task, 0.90);
    }

    // 4. Imperative action verbs in French (seed vocabulary from
    //    benchmark). Keep the list short — false positives are worse
    //    than false negatives at tier 1 (a missed spark escalates to
    //    tier 4; a wrong spark wastes the operator's keep/dismiss
    //    attention).
    if action_verb_re().is_match(first_line) {
        return terminal(Salience::Warm, Addressee::Self_, Actionability::Task, 0.85);
    }

    // 5. Greetings / system pings — addressed at the system itself.
    if greeting_re().is_match(first_line) {
        return terminal(Salience::Hot, Addressee::System, Actionability::Task, 0.85);
    }

    // 6. Trailing `?` — a question. High addressee confidence, lower
    //    salience confidence (some questions are hot, some are
    //    musings). min() falls below the staging threshold, so the
    //    caller escalates despite the recognised shape.
    if trimmed.ends_with('?') {
        let confidences = Confidences {
            salience: 0.60,
            addressee: 0.85,
            actionability: 0.80,
        };
        return ClassifierOutcome {
            axes: Some(Axes {
                salience: Salience::Warm,
                addressee: Addressee::System,
                actionability: Actionability::Task,
            }),
            confidences,
            terminal: confidences.min() >= STAGING_THRESHOLD,
        };
    }

    // Fall through — escalate to tier 4 (v0 has no tier 2/3 yet).
    ClassifierOutcome {
        axes: None,
        confidences: Confidences {
            salience: 0.0,
            addressee: 0.0,
            actionability: 0.0,
        },
        terminal: false,
    }
}

fn terminal(s: Salience, a: Addressee, act: Actionability, conf: f32) -> ClassifierOutcome {
    ClassifierOutcome {
        axes: Some(Axes {
            salience: s,
            addressee: a,
            actionability: act,
        }),
        confidences: Confidences {
            salience: conf,
            addressee: conf,
            actionability: conf,
        },
        terminal: conf >= STAGING_THRESHOLD,
    }
}

// Lazily-compiled regexes. Static init is cheap and avoids re-compile
// on every note.
fn todo_prefix_re() -> &'static Regex {
    static CELL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^(?i)todo\b").expect("regex compiles"))
}
fn action_verb_re() -> &'static Regex {
    static CELL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(
            r"^(?i)(acheter|r[ée]server|trouver|d[ée]finir|nommer|cr[ée]er|[ée]crire|pr[ée]parer|planifier)\b",
        )
        .expect("regex compiles")
    })
}
fn se_renseigner_re() -> &'static Regex {
    static CELL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^(?i)se\s+renseigner\b").expect("regex compiles"))
}
fn greeting_re() -> &'static Regex {
    static CELL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"^(?i)(bonjour|hi|hello|coucou|salut)\b").expect("regex compiles")
    })
}

// ---------------------------------------------------------------------------
// Session-file parsing (read-only; invariant I4 — never opens for write)
// ---------------------------------------------------------------------------

/// One parsed note pulled out of a session markdown file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedNote {
    /// `HH:MM:SS` timestamp.
    pub ts: String,
    /// Optional tag label — empty string if missing.
    pub tag: String,
    /// Trimmed note body.
    pub body: String,
}

/// Extract every `## HH:MM:SS — tag` block from a session file's
/// markdown. Stops at the sealed footer (the opening `---` after the
/// body), mirroring the awk logic of `session-to-spark-tick.sh` so
/// both paths see the same notes.
///
/// The parse is lenient — unrecognised headings are skipped, and an
/// empty body is returned as an empty `body` field (the caller
/// decides whether to filter).
#[must_use]
pub fn parse_session_notes(content: &str) -> Vec<ParsedNote> {
    let mut notes = Vec::new();
    let mut in_fm = false;
    let mut past_fm = false;
    let mut current: Option<ParsedNote> = None;

    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if !in_fm {
                in_fm = true;
                continue;
            }
            if !past_fm {
                past_fm = true;
                continue;
            }
            // Sealed footer opening — flush and stop.
            if let Some(n) = current.take() {
                notes.push(finalize_note(n));
            }
            return notes;
        }
        if !past_fm {
            continue;
        }
        if let Some(header) = line.strip_prefix("## ") {
            if let Some(n) = current.take() {
                notes.push(finalize_note(n));
            }
            let (ts, tag) = parse_note_header(header);
            if ts.is_empty() {
                current = None;
                continue;
            }
            current = Some(ParsedNote {
                ts,
                tag,
                body: String::new(),
            });
            continue;
        }
        if let Some(n) = current.as_mut() {
            if !n.body.is_empty() {
                n.body.push('\n');
            }
            n.body.push_str(line);
        }
    }
    if let Some(n) = current.take() {
        notes.push(finalize_note(n));
    }
    notes
}

/// Strip a `cause:` sub-line and trim the body so downstream hashing
/// is stable against schema evolutions that add new sub-lines.
fn finalize_note(mut n: ParsedNote) -> ParsedNote {
    // Drop a leading `cause:` subline if present — it is metadata, not
    // body content.
    if let Some(rest) = n.body.strip_prefix("cause:") {
        if let Some(idx) = rest.find('\n') {
            n.body = rest[idx + 1..].trim_start_matches('\n').to_owned();
        } else {
            n.body = String::new();
        }
    }
    n.body = n.body.trim().to_owned();
    n
}

fn parse_note_header(header: &str) -> (String, String) {
    // Accept `HH:MM:SS — tag` or `HH:MM:SS -- tag` or bare `HH:MM:SS`.
    let h = header.trim();
    if h.len() < 8 {
        return (String::new(), String::new());
    }
    let (ts_part, rest) = h.split_at(8);
    if !is_hhmmss(ts_part) {
        return (String::new(), String::new());
    }
    let tag = rest
        .trim_start()
        .trim_start_matches(['—', '-'])
        .trim()
        .to_owned();
    (ts_part.to_owned(), tag)
}

fn is_hhmmss(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 8
        && b[2] == b':'
        && b[5] == b':'
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7].is_ascii_digit()
}

/// Extract the `operator` field from the frontmatter (best-effort).
/// Returns an empty string when absent — the caller falls back to
/// git user.name / $USER like `session-to-spark` does.
#[must_use]
pub fn parse_session_operator(content: &str) -> String {
    let mut in_fm = false;
    for line in content.lines() {
        if line.trim_end() == "---" {
            if !in_fm {
                in_fm = true;
                continue;
            }
            return String::new();
        }
        if !in_fm {
            continue;
        }
        if let Some(rest) = line.strip_prefix("operator:") {
            return rest.trim().trim_matches('"').to_owned();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Handler entrypoint — `cs session route`
// ---------------------------------------------------------------------------

/// Resolve the sessions directory honouring the global `--config` flag.
fn sessions_dir(ctx: &Context) -> PathBuf {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    state_dir.join("sessions")
}

fn route_sidecar_dir(sessions: &Path, sid: &str) -> PathBuf {
    sessions.join(".route").join(sid)
}

/// Run `cs session route`. See [`RouteArgs`] for the CLI surface.
///
/// # Errors
/// Propagates I/O, classification, and nucleation errors.
pub fn run(ctx: &Context, args: &RouteArgs) -> anyhow::Result<()> {
    let dir = sessions_dir(ctx);
    if !dir.exists() {
        anyhow::bail!("no .cosmon/state/sessions/ under current project");
    }
    let files = resolve_sessions(&dir, args)?;
    let cosmon_root = cosmon_repo_root_from(&dir);
    let mut summary = TickSummary::default();
    let now = Utc::now();

    for file in &files {
        route_one_session(
            ctx,
            args,
            file,
            &dir,
            cosmon_root.as_deref(),
            now,
            &mut summary,
        )?;
    }

    emit_tick_complete(ctx, args, &summary);
    Ok(())
}

/// Route a single session file. Kept small so `run()` stays under
/// clippy's `too_many_lines` threshold.
fn route_one_session(
    ctx: &Context,
    args: &RouteArgs,
    file: &Path,
    sessions_dir_path: &Path,
    cosmon_root: Option<&Path>,
    now: chrono::DateTime<Utc>,
    summary: &mut TickSummary,
) -> anyhow::Result<()> {
    let sid = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session-unknown")
        .to_owned();
    let content = fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("read session {}: {e}", file.display()))?;
    let operator = parse_session_operator(&content);
    let notes = parse_session_notes(&content);
    summary.found += notes.len();

    let sidecar_dir = route_sidecar_dir(sessions_dir_path, &sid);
    if !args.dry_run {
        fs::create_dir_all(&sidecar_dir)
            .map_err(|e| anyhow::anyhow!("mkdir {}: {e}", sidecar_dir.display()))?;
    }

    for note in notes {
        if summary.emitted >= args.max {
            break;
        }
        route_one_note(
            ctx,
            args,
            &sid,
            &operator,
            &sidecar_dir,
            cosmon_root,
            now,
            &note,
            summary,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn route_one_note(
    ctx: &Context,
    args: &RouteArgs,
    sid: &str,
    operator: &str,
    sidecar_dir: &Path,
    cosmon_root: Option<&Path>,
    now: chrono::DateTime<Utc>,
    note: &ParsedNote,
    summary: &mut TickSummary,
) -> anyhow::Result<()> {
    if note.body.is_empty() {
        summary.empty += 1;
        emit_json_line(
            ctx.json,
            &serde_json::json!({
                "event": "note_skipped",
                "reason": "empty_body",
                "session": sid,
                "note_ts": note.ts,
            }),
        );
        return Ok(());
    }

    let body_hash = Hash::of_bytes(note.body.as_bytes()).to_string();
    let sidecar_path = sidecar_dir.join(format!("blake3-{body_hash}.json"));

    if sidecar_path.exists() {
        if let Ok(existing) = load_sidecar(&sidecar_path) {
            if existing.router_version == ROUTER_VERSION {
                summary.skipped += 1;
                emit_json_line(
                    ctx.json,
                    &serde_json::json!({
                        "event": "note_skipped",
                        "reason": "already_routed",
                        "session": sid,
                        "note_ts": note.ts,
                        "body_hash": body_hash,
                    }),
                );
                return Ok(());
            }
        }
    }

    let outcome = tier1_classify(&note.body);
    let (decided_by, axes, proposed_action) = if outcome.terminal {
        let ax = outcome.axes.expect("terminal outcome has axes");
        (DecidedBy::Tier1, Some(ax), proposed_action_for(ax))
    } else {
        (DecidedBy::Tier4Pending, None, ProposedAction::NeedsYourEye)
    };

    let sidecar = Sidecar {
        note_id: format!("{sid}@{}", note.ts.replace(':', "-")),
        body_hash: format!("blake3:{body_hash}"),
        router_version: ROUTER_VERSION.to_owned(),
        prompt_version: PROMPT_VERSION.to_owned(),
        axes,
        confidences: outcome.confidences,
        proposed_action,
        decided_by,
        decided_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
    };

    if args.dry_run {
        summary.emitted += 1;
        emit_json_line(
            ctx.json,
            &serde_json::json!({
                "event": "note_would_route",
                "session": sid,
                "note_ts": note.ts,
                "body_hash": body_hash,
                "decided_by": decided_by,
                "proposed_action": proposed_action,
                "terminal": outcome.terminal,
            }),
        );
        return Ok(());
    }

    // I3 append-only: on a router bump the file gets a versioned name.
    let target_path = if sidecar_path.exists() {
        sidecar_dir.join(format!("blake3-{body_hash}.{ROUTER_VERSION}.json"))
    } else {
        sidecar_path
    };
    write_sidecar(&target_path, &sidecar)?;
    summary.emitted += 1;

    let staged_id = maybe_stage(
        ctx,
        args,
        &outcome,
        proposed_action,
        cosmon_root,
        note,
        sid,
        operator,
        &body_hash,
        &target_path,
        summary,
    );
    if staged_id.is_some() {
        summary.staged += 1;
    }

    emit_json_line(
        ctx.json,
        &serde_json::json!({
            "event": "note_routed",
            "session": sid,
            "note_ts": note.ts,
            "body_hash": body_hash,
            "decided_by": decided_by,
            "proposed_action": proposed_action,
            "confidence_min": outcome.confidences.min(),
            "confidence_max": outcome.confidences.max(),
            "sidecar": target_path.display().to_string(),
            "staged": staged_id,
        }),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn maybe_stage(
    ctx: &Context,
    args: &RouteArgs,
    outcome: &ClassifierOutcome,
    proposed_action: ProposedAction,
    cosmon_root: Option<&Path>,
    note: &ParsedNote,
    sid: &str,
    operator: &str,
    body_hash: &str,
    target_path: &Path,
    summary: &mut TickSummary,
) -> Option<String> {
    if args.no_stage || !outcome.terminal {
        return None;
    }
    let formula = proposed_action.nucleates_formula()?;
    match stage_molecule(
        cosmon_root,
        formula,
        note,
        sid,
        operator,
        body_hash,
        target_path,
    ) {
        Ok(mol_id) => Some(mol_id),
        Err(e) => {
            summary.stage_failed += 1;
            emit_json_line(
                ctx.json,
                &serde_json::json!({
                    "event": "stage_failed",
                    "session": sid,
                    "note_ts": note.ts,
                    "error": e.to_string(),
                }),
            );
            None
        }
    }
}

fn emit_tick_complete(ctx: &Context, args: &RouteArgs, summary: &TickSummary) {
    emit_json_line(
        ctx.json,
        &serde_json::json!({
            "event": "tick_complete",
            "found": summary.found,
            "emitted": summary.emitted,
            "staged": summary.staged,
            "skipped": summary.skipped,
            "empty": summary.empty,
            "stage_failed": summary.stage_failed,
            "dry_run": args.dry_run,
        }),
    );
    if !ctx.json {
        println!(
            "session-route: found={} routed={} staged={} skipped={} empty={} stage_failed={}{}",
            summary.found,
            summary.emitted,
            summary.staged,
            summary.skipped,
            summary.empty,
            summary.stage_failed,
            if args.dry_run { " (dry-run)" } else { "" }
        );
    }
}

#[derive(Default)]
struct TickSummary {
    found: usize,
    emitted: usize,
    staged: usize,
    skipped: usize,
    empty: usize,
    stage_failed: usize,
}

fn emit_json_line(json_mode: bool, value: &serde_json::Value) {
    if json_mode {
        println!("{value}");
    }
}

fn resolve_sessions(dir: &Path, args: &RouteArgs) -> anyhow::Result<Vec<PathBuf>> {
    if let Some(ref s) = args.session {
        let p = Path::new(s);
        if p.is_file() {
            return Ok(vec![p.to_path_buf()]);
        }
        let candidate = dir.join(format!("{s}.md"));
        if candidate.is_file() {
            return Ok(vec![candidate]);
        }
        anyhow::bail!("session not found: {s}");
    }
    if args.all {
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|e| anyhow::anyhow!("read sessions dir {}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                let stem_ok = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("session-"));
                let ext_ok = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"));
                stem_ok && ext_ok
            })
            .collect();
        files.sort();
        return Ok(files);
    }
    // Default: open session only. Reuse the detection from session.rs.
    let open = super::session::find_open_session(dir)
        .map_err(|e| anyhow::anyhow!("find open session: {e}"))?;
    match open {
        Some(p) => Ok(vec![p]),
        None => Ok(Vec::new()),
    }
}

fn cosmon_repo_root_from(sessions_dir: &Path) -> Option<PathBuf> {
    // `<root>/.cosmon/state/sessions/` — three parents up gives `<root>`.
    sessions_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn load_sidecar(path: &Path) -> anyhow::Result<Sidecar> {
    let raw = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read sidecar {}: {e}", path.display()))?;
    let parsed: Sidecar = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("parse sidecar {}: {e}", path.display()))?;
    Ok(parsed)
}

fn write_sidecar(path: &Path, sidecar: &Sidecar) -> anyhow::Result<()> {
    // Canonicalise with pretty JSON (sorted keys would be the gold
    // standard — but serde_json::to_string_pretty on a struct with
    // consistent field order is already byte-stable for I2).
    let body = serde_json::to_string_pretty(sidecar)
        .map_err(|e| anyhow::anyhow!("serialize sidecar: {e}"))?;
    let mut f = fs::File::create(path)
        .map_err(|e| anyhow::anyhow!("create sidecar {}: {e}", path.display()))?;
    f.write_all(body.as_bytes())
        .map_err(|e| anyhow::anyhow!("write sidecar: {e}"))?;
    f.write_all(b"\n")
        .map_err(|e| anyhow::anyhow!("write sidecar trailing newline: {e}"))?;
    Ok(())
}

fn stage_molecule(
    cosmon_root: Option<&Path>,
    formula: &str,
    note: &ParsedNote,
    sid: &str,
    operator: &str,
    body_hash: &str,
    sidecar: &Path,
) -> anyhow::Result<String> {
    let ts_tag = note.ts.replace(':', "-");
    let relative_sidecar = sidecar
        .strip_prefix(cosmon_root.unwrap_or_else(|| Path::new(".")))
        .unwrap_or(sidecar)
        .to_path_buf();

    let mut cmd = Command::new("cs");
    cmd.arg("--json");
    if let Some(root) = cosmon_root {
        cmd.current_dir(root);
    }
    cmd.arg("nucleate").arg(formula);
    cmd.args(["--var", &format!("topic={}", note.body)]);
    if !operator.is_empty() {
        cmd.args(["--var", &format!("nucleon_id={operator}")]);
    }
    cmd.args(["--var", &format!("session_id={sid}")]);
    cmd.args(["--var", &format!("note_timestamp={}", note.ts)]);
    cmd.args(["--var", &format!("body_hash={body_hash}")]);
    cmd.args(["--var", &format!("sidecar={}", relative_sidecar.display())]);
    if !note.tag.is_empty() {
        cmd.args(["--var", &format!("note_tag={}", note.tag)]);
    }
    cmd.args(["--tag", "temp:proposed"]);
    cmd.args(["--tag", "source:session"]);
    cmd.args(["--tag", "stream:session-route"]);
    cmd.args(["--tag", &format!("session-note:{sid}@{ts_tag}")]);
    cmd.arg("--no-parent");

    let out = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("spawn cs nucleate: {e}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "cs nucleate exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Parse the JSON NDJSON (first line carries `id`).
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
            return Ok(id.to_owned());
        }
    }
    anyhow::bail!("no molecule id in nucleate output");
}

impl std::fmt::Display for DecidedBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Tier1 => "tier1",
            Self::Tier2Local => "tier2_local",
            Self::Tier3Cloud => "tier3_cloud",
            Self::Tier4Pending => "tier4_pending",
            Self::Tier4Resolved => "tier4_resolved",
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // The 11-note benchmark corpus (bodies only). Order mirrors the
    // ADR-072 table.
    #[allow(clippy::type_complexity)]
    const CORPUS: [(usize, &str, Option<(Salience, Addressee, Actionability)>); 11] = [
        (
            1,
            "Ou en est le developpement de cosmon?",
            Some((Salience::Warm, Addressee::System, Actionability::Task)),
        ),
        (2, "dystopie: comment achète-t-on une baguette", None),
        (3, "plusieurs idées", None),
        (
            4,
            "nommer la communauté Noogram — trouver un nom juste",
            Some((Salience::Warm, Addressee::Self_, Actionability::Task)),
        ),
        (
            5,
            "définir un process d'onboarding: NDA, YubiKey, accès",
            Some((Salience::Warm, Addressee::Self_, Actionability::Task)),
        ),
        (
            6,
            "7 anneaux Tolkien pour les vetoers?",
            Some((Salience::Warm, Addressee::System, Actionability::Task)),
        ),
        (
            7,
            "github noogram-labs réservé — faire valider juridique?",
            Some((Salience::Warm, Addressee::System, Actionability::Task)),
        ),
        (
            8,
            "Multiplex de voix — plusieurs personae parlent en parallèle dans la conv. On entend une symphonie.",
            None,
        ),
        (
            9,
            "si validé dans l'ux, le multiplex devient le mode par défaut",
            None,
        ),
        (
            10,
            "se renseigner sur la xbox portable",
            Some((Salience::Warm, Addressee::Self_, Actionability::Task)),
        ),
        (
            11,
            "galaxie tenant-demo sur noogram ou noogram-labs?",
            Some((Salience::Warm, Addressee::System, Actionability::Task)),
        ),
    ];

    #[test]
    fn corpus_meets_five_of_eleven_floor() {
        let mut matched = 0usize;
        let mut wrong = 0usize;
        for (idx, body, expected) in CORPUS {
            let out = tier1_classify(body);
            match (out.axes, expected) {
                (Some(got), Some((s, a, act))) => {
                    if got.salience == s && got.addressee == a && got.actionability == act {
                        matched += 1;
                    } else {
                        wrong += 1;
                        eprintln!(
                            "note #{idx} mis-labelled: got {got:?}, wanted ({s:?}, {a:?}, {act:?})"
                        );
                    }
                }
                (Some(got), None) => {
                    wrong += 1;
                    eprintln!("note #{idx} false positive: got {got:?}, wanted escalation");
                }
                (None, Some((s, a, act))) => {
                    eprintln!(
                        "note #{idx} false negative (escalated): wanted ({s:?}, {a:?}, {act:?})"
                    );
                }
                (None, None) => { /* correctly escalated */ }
            }
        }
        assert!(
            matched >= 5,
            "tier-1 regex must label ≥5/11 notes (matched={matched}, wrong={wrong})"
        );
        // ADR-072 §... "zero false sparks" — precision floor: no note
        // mis-classified to a concrete axis triple.
        assert_eq!(wrong, 0, "tier-1 must not mis-classify — got {wrong} wrong");
    }

    #[test]
    fn note_3_plusieurs_idees_stays_orphan() {
        // The "plusieurs idées" note is the canonical orphan — must
        // escalate (terminal = false, axes = None).
        let out = tier1_classify("plusieurs idées");
        assert!(out.axes.is_none(), "orphan note must escalate");
        assert!(!out.terminal);
    }

    #[test]
    fn spark_prefix_is_tier1_terminal_with_confidence_1() {
        let out = tier1_classify("!spark buy milk");
        assert!(out.terminal);
        let ax = out.axes.unwrap();
        assert_eq!(ax.salience, Salience::Hot);
        assert_eq!(ax.addressee, Addressee::Self_);
        assert_eq!(ax.actionability, Actionability::Task);
        assert!((out.confidences.min() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn todo_prefix_maps_to_task() {
        let out = tier1_classify("TODO: call the dentist");
        assert!(out.terminal);
        let ax = out.axes.unwrap();
        assert_eq!(ax.actionability, Actionability::Task);
    }

    #[test]
    fn se_renseigner_maps_to_warm_task() {
        let out = tier1_classify("se renseigner sur la xbox portable");
        let ax = out.axes.unwrap();
        assert_eq!(ax.salience, Salience::Warm);
        assert_eq!(ax.actionability, Actionability::Task);
    }

    #[test]
    fn question_mark_is_addressee_system_but_not_terminal() {
        // A `?` alone does not cross the staging threshold — we
        // recognise the shape but still escalate so the operator
        // confirms.
        let out = tier1_classify("Ou en est le developpement de cosmon?");
        let ax = out.axes.unwrap();
        assert_eq!(ax.addressee, Addressee::System);
        // Confidence is below staging — we do not auto-stage a question.
        assert!(!out.terminal);
    }

    #[test]
    fn greeting_is_hot_system_task() {
        let out = tier1_classify("bonjour, où en est-on ?");
        let ax = out.axes.unwrap();
        assert_eq!(ax.salience, Salience::Hot);
        assert_eq!(ax.addressee, Addressee::System);
    }

    #[test]
    fn proposed_action_maps_axes_to_destination() {
        let spark = Axes {
            salience: Salience::Hot,
            addressee: Addressee::Self_,
            actionability: Actionability::Task,
        };
        assert_eq!(proposed_action_for(spark), ProposedAction::NucleateSpark);

        let question = Axes {
            salience: Salience::Warm,
            addressee: Addressee::System,
            actionability: Actionability::Task,
        };
        assert_eq!(
            proposed_action_for(question),
            ProposedAction::NucleateQuestion
        );

        let chronicle = Axes {
            salience: Salience::Warm,
            addressee: Addressee::Self_,
            actionability: Actionability::Reflection,
        };
        assert_eq!(
            proposed_action_for(chronicle),
            ProposedAction::AppendChronicle
        );

        let cold = Axes {
            salience: Salience::Cold,
            addressee: Addressee::Self_,
            actionability: Actionability::Task,
        };
        assert_eq!(proposed_action_for(cold), ProposedAction::Skip);
    }

    #[test]
    fn session_parser_extracts_notes() {
        let content = "---\n\
session_id: session-x\n\
operator: you\n\
---\n\
\n\
## 10:00:00 — insight\n\
\n\
first body\n\
\n\
## 10:01:00 — \n\
\n\
second body line 1\n\
second body line 2\n\
\n";
        let notes = parse_session_notes(content);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].ts, "10:00:00");
        assert_eq!(notes[0].tag, "insight");
        assert_eq!(notes[0].body, "first body");
        assert_eq!(notes[1].ts, "10:01:00");
        assert!(notes[1].body.contains("second body line 1"));
        assert!(notes[1].body.contains("second body line 2"));
    }

    #[test]
    fn session_parser_stops_at_sealed_footer() {
        let content = "---\na: 1\n---\n\n## 10:00:00 — \n\nhello\n\n---\nended_at: X\n---\n";
        let notes = parse_session_notes(content);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "hello");
    }

    #[test]
    fn session_parser_drops_cause_subline() {
        let content = "---\na: 1\n---\n\n## 10:00:00 — \ncause: direct/keyboard\n\nhello\n\n";
        let notes = parse_session_notes(content);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "hello");
    }

    #[test]
    fn session_parser_extracts_operator() {
        let content = "---\nsession_id: x\noperator: \"you\"\n---\n\n## 10:00:00 — \n\nb\n\n";
        assert_eq!(parse_session_operator(content), "you");
    }

    #[test]
    fn classifier_is_pure_byte_identical() {
        // I2: given identical input, identical output. We serialize
        // the outcome and check the JSON is byte-stable.
        let body = "se renseigner sur la xbox portable";
        let a = tier1_classify(body);
        let b = tier1_classify(body);
        let a_json = serde_json::to_string(&a.axes).unwrap();
        let b_json = serde_json::to_string(&b.axes).unwrap();
        assert_eq!(a_json, b_json);
        assert!((a.confidences.min() - b.confidences.min()).abs() < f32::EPSILON);
    }

    #[test]
    fn body_hash_stable_and_distinct() {
        // I1: body-primacy. Same body → same hash; different body →
        // different hash.
        let h1 = Hash::of_bytes(b"hello").to_string();
        let h2 = Hash::of_bytes(b"hello").to_string();
        let h3 = Hash::of_bytes(b"world").to_string();
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64); // blake3 hex
    }

    #[test]
    fn corpus_zero_false_sparks() {
        // Precision check: a false positive (regex mis-classifies an
        // ambiguous note) is strictly worse than a false negative.
        // Spot-check the dangerous bodies.
        let dangerous = [
            "plusieurs idées",                            // orphan
            "dystopie: comment achète-t-on une baguette", // narrative
            "Multiplex de voix — plusieurs personae parlent",
        ];
        for body in dangerous {
            let out = tier1_classify(body);
            assert!(
                !out.terminal,
                "dangerous body {body:?} classified terminal — false positive"
            );
        }
    }

    #[test]
    fn sidecar_roundtrip_preserves_schema() {
        // I2 byte-stability on the sidecar payload itself.
        let sc = Sidecar {
            note_id: "session-x@10-00-00".to_owned(),
            body_hash: "blake3:abcd".to_owned(),
            router_version: ROUTER_VERSION.to_owned(),
            prompt_version: PROMPT_VERSION.to_owned(),
            axes: Some(Axes {
                salience: Salience::Hot,
                addressee: Addressee::Self_,
                actionability: Actionability::Task,
            }),
            confidences: Confidences {
                salience: 0.9,
                addressee: 0.9,
                actionability: 0.9,
            },
            proposed_action: ProposedAction::NucleateSpark,
            decided_by: DecidedBy::Tier1,
            decided_at: "2026-04-24T09:00:00Z".to_owned(),
        };
        let a = serde_json::to_string_pretty(&sc).unwrap();
        let b = serde_json::to_string_pretty(&sc).unwrap();
        assert_eq!(a, b, "sidecar serialization must be byte-stable");
        let parsed: Sidecar = serde_json::from_str(&a).unwrap();
        assert_eq!(parsed.body_hash, sc.body_hash);
    }
}
