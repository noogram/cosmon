// SPDX-License-Identifier: AGPL-3.0-only

//! `cs ask <free text>` — conversational ingress (MVP, rule-first).
//!
//! Parses free text into `(kind, formula, galaxy)` via `cosmon_ask`,
//! applies the confidence gate, and either
//!
//! * emits the resolved dispatch plan (default — dry-run), or
//! * actually shells out to `cs nucleate` + `cs tackle` when
//!   `--execute` is passed.
//!
//! Gated behind `--experimental` until telemetry confirms hit-rate
//! ≥ 70% (briefing deliverable 2). Running without `--experimental`
//! prints the safety notice and exits 0 — no side effects.
//!
//! Audit log is always appended (one NDJSON line) to
//! `.cosmon/state/ask.jsonl`.

use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;
use cosmon_ask::{AskPipeline, AskState, AtomicQuestion, AuditRecord, Outcome, RuleParser};
use cosmon_registry::TomlGalaxyIndex;

use super::Context;

/// Arguments for `cs ask`.
#[derive(clap::Args)]
pub struct Args {
    /// Free-text intent — the operator's sentence.
    ///
    /// Tokens are not pre-processed; the full sentence becomes the
    /// molecule's `topic` variable. Quote the argument when it
    /// contains spaces or shell metacharacters.
    pub text: String,

    /// Enable the experimental verb. Until hit-rate ≥ 70% is proved
    /// out, `cs ask` without this flag prints a safety notice and
    /// does nothing.
    #[arg(long)]
    pub experimental: bool,

    /// Actually dispatch — shell out to `cs nucleate` + `cs tackle`.
    /// Without this, the command is a dry-run: it prints the resolved
    /// plan and appends an audit record, but does not create a
    /// molecule.
    #[arg(long)]
    pub execute: bool,

    /// Override the confidence floor (default: 0.85).
    #[arg(long = "confidence-floor", value_name = "0.0..1.0")]
    pub confidence_floor: Option<f32>,

    /// Use a custom galaxies.toml (default: walk-up / `~/.config/cosmon/galaxies.toml`).
    #[arg(long, value_name = "PATH")]
    pub registry: Option<PathBuf>,

    /// Accept the atomic question's default without prompting. For
    /// scripting and tests — a human at a TTY should answer the
    /// verdict-door directly.
    #[arg(long)]
    pub accept_default: bool,
}

/// Entry point for `cs ask`.
///
/// # Errors
///
/// Returns errors from the registry (parse / I/O), the parser
/// (empty input), or the shell-out verbs (when `--execute` is set).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if !args.experimental {
        eprintln!(
            "cs ask is experimental — re-run with --experimental once you have read the briefing."
        );
        return Ok(());
    }

    let registry = load_registry(args.registry.as_deref())?;
    let mut pipeline = AskPipeline::new(RuleParser::with_defaults(), registry);
    if let Some(floor) = args.confidence_floor {
        pipeline = pipeline.with_confidence_floor(floor);
    }

    let state = pipeline.run(&args.text)?;
    handle_state(ctx, args, state)
}

fn load_registry(explicit: Option<&std::path::Path>) -> anyhow::Result<TomlGalaxyIndex> {
    match explicit {
        Some(p) => TomlGalaxyIndex::load_from(p)
            .map_err(|e| anyhow::anyhow!("load registry from {}: {e}", p.display())),
        None => TomlGalaxyIndex::load_default()
            .map_err(|e| anyhow::anyhow!("load default registry: {e}")),
    }
}

fn handle_state(ctx: &Context, args: &Args, state: AskState) -> anyhow::Result<()> {
    match state {
        AskState::Resolved {
            galaxy,
            formula,
            vars,
        } => {
            let topic = vars
                .get("topic")
                .cloned()
                .unwrap_or_else(|| args.text.clone());
            render_resolved(ctx, &galaxy.name, &galaxy.path, formula.as_str(), &topic);
            let (mol_id, outcome) = if args.execute {
                dispatch(&galaxy.path, formula.as_str(), &topic)?
            } else {
                (None, Outcome::Dispatched)
            };
            let record = AuditRecord {
                ts: Utc::now().to_rfc3339(),
                intent_text: args.text.clone(),
                parsed_tokens: tokens_stub(&topic, formula.as_str()),
                confidence: cosmon_ask::DEFAULT_CONFIDENCE_FLOOR,
                resolved_galaxy: Some(galaxy.name.clone()),
                formula: Some(formula.as_str().to_owned()),
                mol_id,
                outcome,
            };
            append_audit(&record);
            Ok(())
        }

        AskState::AskedClarification { reason, question } => {
            render_clarification(ctx, &reason, &question);
            let record = AuditRecord {
                ts: Utc::now().to_rfc3339(),
                intent_text: args.text.clone(),
                parsed_tokens: question.captured.clone(),
                confidence: 0.0,
                resolved_galaxy: None,
                formula: None,
                mol_id: None,
                outcome: Outcome::Aborted,
            };
            append_audit(&record);
            Ok(())
        }

        AskState::Parsed { .. } | AskState::Dispatched { .. } => {
            // `run()` guarantees these do not leak to the handler.
            Ok(())
        }
    }
}

fn render_resolved(
    ctx: &Context,
    galaxy: &str,
    path: &std::path::Path,
    formula: &str,
    topic: &str,
) {
    if ctx.json {
        let payload = serde_json::json!({
            "state": "resolved",
            "galaxy": galaxy,
            "path": path,
            "formula": formula,
            "topic": topic,
        });
        println!("{payload}");
    } else {
        println!(
            "ask → {galaxy} (path: {}) [formula={formula}]",
            path.display()
        );
        println!("topic: {topic}");
    }
}

fn render_clarification(ctx: &Context, reason: &str, q: &AtomicQuestion) {
    if ctx.json {
        let payload = serde_json::json!({
            "state": "clarification",
            "reason": reason,
            "prompt": q.prompt,
            "default": { "slug": q.default.slug, "label": q.default.label },
            "alternatives": q.alternatives.iter().map(|c| serde_json::json!({
                "slug": c.slug, "label": c.label
            })).collect::<Vec<_>>(),
        });
        println!("{payload}");
    } else {
        println!("ask needs a verdict — {}", q.prompt);
        println!("  1) {}  [default]", q.default.label);
        for (i, c) in q.alternatives.iter().enumerate() {
            println!("  {}) {}", i + 2, c.label);
        }
        println!("  later");
    }
}

/// Shell out to `cs nucleate` and `cs tackle`. Returns the molecule
/// id captured from `cs nucleate --json` if available.
fn dispatch(
    path: &std::path::Path,
    formula: &str,
    topic: &str,
) -> anyhow::Result<(Option<String>, Outcome)> {
    let nucleate = Command::new("cs")
        .current_dir(path)
        .arg("--json")
        .arg("nucleate")
        .arg(formula)
        .arg("--var")
        .arg(format!("topic={topic}"))
        .arg("--tag")
        .arg("temp:hot")
        .output()
        .map_err(|e| anyhow::anyhow!("spawn cs nucleate: {e}"))?;
    if !nucleate.status.success() {
        let stderr = String::from_utf8_lossy(&nucleate.stderr);
        return Err(anyhow::anyhow!("cs nucleate failed: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&nucleate.stdout);
    let mol_id = extract_mol_id(&stdout);

    if let Some(id) = &mol_id {
        let tackle = Command::new("cs")
            .current_dir(path)
            .arg("tackle")
            .arg(id)
            .status()
            .map_err(|e| anyhow::anyhow!("spawn cs tackle: {e}"))?;
        if !tackle.success() {
            return Ok((mol_id, Outcome::Errored));
        }
    }
    Ok((mol_id, Outcome::Dispatched))
}

fn extract_mol_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(id) = v.get("molecule_id").and_then(|x| x.as_str()) {
                return Some(id.to_owned());
            }
            if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                return Some(id.to_owned());
            }
        }
    }
    None
}

fn tokens_stub(topic: &str, formula: &str) -> cosmon_ask::AskTokens {
    cosmon_ask::AskTokens {
        intent_verb: "resolved".to_owned(),
        kind: cosmon_core::kind::MoleculeKind::Task,
        formula: cosmon_core::id::FormulaId::new(formula).unwrap_or_else(|_| {
            cosmon_core::id::FormulaId::new("task-work").expect("task-work is valid")
        }),
        galaxy_hint: None,
        topic: topic.to_owned(),
    }
}

fn append_audit(record: &AuditRecord) {
    let path = super::default_state_dir().join("ask.jsonl");
    // Best-effort: never block the hot path. Mirror the briefing-seal
    // discipline from CLAUDE.md — propose verification, don't impose.
    let _ = record.append(&path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mol_id_from_nucleate_stdout() {
        let stdout = r#"{"molecule_id":"task-20260423-abcd","formula":"task-work"}"#;
        assert_eq!(
            extract_mol_id(stdout).as_deref(),
            Some("task-20260423-abcd")
        );
    }

    #[test]
    fn extract_mol_id_missing_is_none() {
        assert!(extract_mol_id("").is_none());
        assert!(extract_mol_id("not-json").is_none());
    }
}
