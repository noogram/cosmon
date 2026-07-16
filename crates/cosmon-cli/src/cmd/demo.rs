// SPDX-License-Identifier: AGPL-3.0-only

//! `cs demo` — one-command, end-to-end chatbot-like surface for a first-contact
//! user experience.
//!
//! `cs demo` is a thin orchestrator above the canonical `nucleate → tackle →
//! wait → done` trinity. It asks for a prompt, classifies it, runs the full
//! pipeline in the background, renders the synthesis, and tears everything
//! down — all in a single invocation.
//!
//! # Architectural discipline (ADR-016)
//!
//! `cs demo` respects every invariant:
//!
//! - **Stateless**: no daemon, no background thread survives the process; a
//!   single foreground run that walks the existing verbs and exits.
//! - **Zero new state**: all artefacts still live under `.cosmon/state/…`
//!   exactly as with the manual cycle — the demo adds no private surface.
//! - **Composable with existing formulas**: the formula is chosen from the
//!   already-installed set (`task-work`, `deep-think`, `idea-to-plan`); there
//!   is no "demo formula type".
//! - **CLI over MCP**: the orchestration re-invokes `cs` as a subprocess via
//!   [`std::env::current_exe`], so every gate already enforced on the
//!   individual verbs applies transitively.
//!
//! # Wedge
//!
//! The goal is to compress the "time to first wow" from ~30 minutes of reading
//! `THESIS.md` + manual setup to a single `cs demo` command. The chat-bot
//! surface is a *disguise*: underneath, the cycle is exactly the same one a
//! worker runs. Artefacts persist; the user can re-inspect via `cs peek`.
//!
//! # Cost & backend
//!
//! `cs demo` dispatches a **real** worker, so it needs a reachable model
//! backend. With no `--adapter` it uses the built-in
//! [`BUILTIN_FLOOR_ADAPTER`](cosmon_core::config::BUILTIN_FLOOR_ADAPTER)
//! (`"local"`, the Ollama-backed in-process loop) — **no hosted model, no API
//! key, no spend**. A paid backend runs *only* when the operator explicitly
//! opts in (`--adapter claude`/`openai`, `$COSMON_DEFAULT_ADAPTER`, or an
//! `[adapters.default]` in config). The demo never silently falls back to a
//! billed provider. For a zero-key, zero-cost taste with no backend at all,
//! `examples/hello-notarized` runs the notarize primitive offline in ~4s.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use colored::Colorize;

use super::Context;

/// Arguments for the `demo` subcommand.
///
/// All fields are optional — the default is *interactive mode*: read a prompt
/// from stdin, classify it, run `deep-think` (or a formula inferred from the
/// prompt shape), render the synthesis, tear down.
#[derive(clap::Args, Debug, Default)]
pub struct Args {
    /// Skip the interactive prompt; use this text as the demo input.
    #[arg(long)]
    pub prompt: Option<String>,

    /// Force a specific formula instead of auto-classifying.
    ///
    /// The named formula must already exist under `.cosmon/formulas/`. No new
    /// formulas are registered by `cs demo`.
    #[arg(long)]
    pub formula: Option<String>,

    /// Skip the final `cs done` teardown — useful for debugging.
    ///
    /// When set, the molecule remains Completed (or Collapsed) but its
    /// worktree, tmux session, and fleet worker are left intact for
    /// post-mortem inspection.
    #[arg(long)]
    pub no_teardown: bool,

    /// Maximum seconds to wait for the molecule to reach a terminal state.
    ///
    /// Mirrors the `cs wait --timeout` default so `cs demo` does not silently
    /// allow a runaway demo to hang the operator's terminal.
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,

    /// Worker-Spawn Port Adapter to dispatch (ADR-079 / ADR-097 / ADR-106).
    ///
    /// Mirrors `cs tackle --adapter`: when set, the value is threaded through
    /// to the `cs tackle` invocation that `cs demo` spawns under the hood, so
    /// the demo cycle can exercise any registered Adapter (e.g. `llama-cpp`,
    /// `claude`, `aider`, `openai-chat`). Optional — the default resolution
    /// path (`.cosmon/config.toml::[adapters.default]` → built-in
    /// [`BUILTIN_FLOOR_ADAPTER`](cosmon_core::config::BUILTIN_FLOOR_ADAPTER),
    /// currently `"local"`, the Ollama-backed in-process loop — never Claude)
    /// is preserved when the flag is omitted.
    ///
    /// Per ADR-106 the canonical name for the in-process llama.cpp adapter
    /// is `llama-cpp`; the legacy alias `llama` is accepted at the CLI seam
    /// (canonicalises via `cs tackle`'s `validate_adapter_name`).
    #[arg(long, value_name = "NAME")]
    pub adapter: Option<String>,
}

/// Run the `demo` subcommand end-to-end.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let started = Instant::now();
    let prompt = resolve_prompt(args)?;
    let formula = args
        .formula
        .clone()
        .unwrap_or_else(|| classify_prompt(&prompt));
    let var_key = formula_variable_key(&formula);

    emit_event(
        ctx,
        "demo_start",
        &[("formula", formula.as_str()), ("prompt", prompt.as_str())],
    );

    banner_human(ctx, &formula, &prompt);

    let mol_id = step_nucleate(ctx, &formula, var_key, &prompt)?;
    emit_event(ctx, "nucleated", &[("molecule", mol_id.as_str())]);
    progress_line(ctx, &format!("nucleated {mol_id}"));

    step_tackle(ctx, &mol_id, args.adapter.as_deref())?;
    let mut tackled_fields = vec![("molecule", mol_id.as_str())];
    if let Some(name) = args.adapter.as_deref() {
        tackled_fields.push(("adapter", name));
    }
    emit_event(ctx, "tackled", &tackled_fields);
    progress_line(ctx, &format!("tackled {mol_id} (worker dispatched)"));

    step_wait(ctx, &mol_id, args.timeout)?;
    emit_event(ctx, "completed", &[("molecule", mol_id.as_str())]);

    render_synthesis(ctx, &mol_id)?;

    if !args.no_teardown {
        step_done(ctx, &mol_id)?;
        emit_event(ctx, "torndown", &[("molecule", mol_id.as_str())]);
        progress_line(ctx, &format!("torn down {mol_id} (branch merged)"));
    }

    let elapsed = started.elapsed();
    emit_event(
        ctx,
        "demo_done",
        &[
            ("molecule", mol_id.as_str()),
            ("elapsed_seconds", &format!("{:.1}", elapsed.as_secs_f64())),
        ],
    );

    if !ctx.json {
        println!(
            "\n{} demo finished in {:.1}s — molecule {} preserved at {}",
            "✨".bold(),
            elapsed.as_secs_f64(),
            mol_id.as_str().bold(),
            format!(".cosmon/state/**/{mol_id}/").dimmed(),
        );
    }
    Ok(())
}

/// Resolve the demo prompt: explicit `--prompt`, or interactive stdin.
///
/// In a non-TTY environment with no `--prompt`, we fail fast rather than
/// hanging on a `read_line` call that will never return.
fn resolve_prompt(args: &Args) -> anyhow::Result<String> {
    if let Some(p) = &args.prompt {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            anyhow::bail!("--prompt is empty");
        }
        return Ok(trimmed.to_owned());
    }
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        anyhow::bail!("no --prompt provided and stdin is not a TTY; pass --prompt \"…\"");
    }
    // One-line, no-surprise notice before we read the prompt: the demo needs a
    // model backend, and it never spends on a hosted model unless the operator
    // opted in. Kept to a single dimmed line so it does not bury the prompt.
    if args.adapter.is_none() {
        eprintln!(
            "{}",
            "cs demo drives the built-in 'local' adapter (Ollama on localhost:11434) — \
             no key, no spend. Pass --adapter claude/openai to use a hosted model."
                .dimmed()
        );
    }
    // Chatbot-style prompt. Keep it simple — a single line is enough for
    // the wedge; multi-line editing can come later.
    print!("{} ", "›".bold().cyan());
    let _ = io::stdout().flush();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty prompt");
    }
    Ok(trimmed.to_owned())
}

/// Classify a prompt into a formula id using heuristics.
///
/// This is deliberately crude — the goal is a sensible default, not
/// accuracy. Operators who disagree override with `--formula`.
pub(crate) fn classify_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    let lower = trimmed.to_lowercase();

    let looks_like_question = trimmed.ends_with('?')
        || lower.starts_with("what ")
        || lower.starts_with("why ")
        || lower.starts_with("how ")
        || lower.starts_with("should ")
        || lower.starts_with("is ")
        || lower.starts_with("are ")
        || lower.starts_with("est-ce")
        || lower.starts_with("pourquoi")
        || lower.starts_with("comment ");
    if looks_like_question {
        return "deep-think".to_owned();
    }

    let imperative_starts = [
        "implement",
        "add",
        "fix",
        "refactor",
        "build",
        "write",
        "create",
        "update",
        "remove",
        "delete",
        "rename",
    ];
    if imperative_starts.iter().any(|v| lower.starts_with(v)) {
        return "task-work".to_owned();
    }

    "idea-to-plan".to_owned()
}

/// Canonical variable name for a formula's principal free parameter.
///
/// Mirrors the ad-hoc conventions in the existing formulas (`question` for
/// deliberations, `topic` for everything else). Extracted so tests can keep
/// this mapping in sync without parsing TOML.
pub(crate) fn formula_variable_key(formula: &str) -> &'static str {
    match formula {
        "deep-think" => "question",
        _ => "topic",
    }
}

/// Emit a single NDJSON line on stdout when `--json` is set.
fn emit_event(ctx: &Context, event: &str, fields: &[(&str, &str)]) {
    if !ctx.json {
        return;
    }
    let mut map = serde_json::Map::new();
    map.insert(
        "event".to_owned(),
        serde_json::Value::String(event.to_owned()),
    );
    for (k, v) in fields {
        map.insert((*k).to_owned(), serde_json::Value::String((*v).to_owned()));
    }
    if let Ok(line) = serde_json::to_string(&serde_json::Value::Object(map)) {
        println!("{line}");
    }
}

fn banner_human(ctx: &Context, formula: &str, prompt: &str) {
    if ctx.json {
        return;
    }
    println!(
        "{} {} — formula {}",
        "▶".bold().cyan(),
        "cs demo".bold(),
        formula.bold().green(),
    );
    println!("{} {}", "❯".dimmed(), prompt);
    println!();
}

fn progress_line(ctx: &Context, msg: &str) {
    if ctx.json {
        return;
    }
    println!("  {} {}", "⏳".bold(), msg.dimmed());
}

/// Locate the `cs` binary to re-invoke. Falls back to the literal name so
/// tests that rely on `$PATH` discovery keep working even when
/// `current_exe` points to a harness wrapper.
fn cs_binary() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cs"))
}

/// Run `cs <verb> …` inheriting stdio for humans, capturing stdout for
/// machine-readable responses. Fails if the child exits non-zero.
fn spawn_with_inherited_stderr(
    ctx: &Context,
    args: &[&str],
    capture_stdout: bool,
) -> anyhow::Result<std::process::Output> {
    let mut cmd = Command::new(cs_binary());
    cmd.args(args);
    if let Some(cfg) = &ctx.config {
        cmd.arg("--config").arg(cfg);
    }
    if capture_stdout {
        cmd.stdout(Stdio::piped());
    } else {
        cmd.stdout(Stdio::inherit());
    }
    cmd.stderr(Stdio::inherit());
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "`cs {}` exited with {}",
            args.first().copied().unwrap_or("?"),
            output.status
        );
    }
    Ok(output)
}

fn step_nucleate(
    ctx: &Context,
    formula: &str,
    var_key: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    let var = format!("{var_key}={prompt}");
    let args = vec![
        "nucleate",
        formula,
        "--var",
        var.as_str(),
        "--no-parent",
        "--json",
    ];
    let out = spawn_with_inherited_stderr(ctx, &args, true)?;
    // nucleate emits a single JSON object on stdout.
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse `cs nucleate --json` output: {e}"))?;
    let id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("`cs nucleate --json` missing `id`"))?
        .to_owned();
    Ok(id)
}

fn step_tackle(ctx: &Context, mol_id: &str, adapter: Option<&str>) -> anyhow::Result<()> {
    let args = build_tackle_args(mol_id, adapter);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn_with_inherited_stderr(ctx, &arg_refs, ctx.json)?;
    Ok(())
}

/// Compose the argv that `cs demo` hands to `cs tackle`.
///
/// Extracted so unit tests can pin the dispatch shape — in particular,
/// that `--adapter <name>` is threaded through verbatim (ADR-106 C4):
/// the canonicalisation lives in `cs tackle`'s `validate_adapter_name`,
/// not in the demo orchestrator, so the alias (`llama` → `llama-cpp`)
/// path is exercised by the same code on both invocation surfaces.
pub(crate) fn build_tackle_args(mol_id: &str, adapter: Option<&str>) -> Vec<String> {
    let mut args = vec!["tackle".to_owned(), mol_id.to_owned()];
    if let Some(name) = adapter {
        args.push("--adapter".to_owned());
        args.push(name.to_owned());
    }
    args
}

fn step_wait(ctx: &Context, mol_id: &str, timeout: u64) -> anyhow::Result<()> {
    let timeout_s = timeout.to_string();
    let args: Vec<&str> = if ctx.json {
        vec!["wait", mol_id, "--timeout", &timeout_s, "--quiet"]
    } else {
        vec!["wait", mol_id, "--timeout", &timeout_s]
    };
    // In JSON mode we swallow the final wait payload — we'll emit our own
    // `completed` event instead.
    spawn_with_inherited_stderr(ctx, &args, ctx.json)?;
    Ok(())
}

fn step_done(ctx: &Context, mol_id: &str) -> anyhow::Result<()> {
    spawn_with_inherited_stderr(ctx, &["done", mol_id], ctx.json)?;
    Ok(())
}

/// Locate the molecule's artefact directory and print the highest-fidelity
/// artefact we can find (`synthesis.md` > `briefing.md` > `prompt.md`).
///
/// We resolve via walk-up discovery (same logic other commands use) then
/// glob all fleets — this keeps us honest against the fleet-scoped layout
/// and future multi-fleet projects.
fn render_synthesis(ctx: &Context, mol_id: &str) -> anyhow::Result<()> {
    let Some(dir) = locate_molecule_dir(ctx, mol_id) else {
        if !ctx.json {
            eprintln!(
                "cs demo: could not locate molecule directory for {mol_id}; \
                 inspect with `cs peek {mol_id}`"
            );
        }
        return Ok(());
    };
    let candidates = ["synthesis.md", "briefing.md", "prompt.md"];
    for name in candidates {
        let path = dir.join(name);
        if path.is_file() {
            if ctx.json {
                emit_event(
                    ctx,
                    "artefact",
                    &[
                        ("molecule", mol_id),
                        ("file", name),
                        ("path", path.to_string_lossy().as_ref()),
                    ],
                );
                return Ok(());
            }
            print_markdown_file(&path, name)?;
            return Ok(());
        }
    }
    if !ctx.json {
        eprintln!(
            "cs demo: {} has no rendered artefact yet (looked for synthesis.md, \
             briefing.md, prompt.md in {})",
            mol_id,
            dir.display(),
        );
    }
    Ok(())
}

/// Walk the on-disk fleet layout and return the first matching molecule
/// directory. The state root is discovered the same way every other
/// command does (explicit `--config`, else walk-up).
fn locate_molecule_dir(ctx: &Context, mol_id: &str) -> Option<PathBuf> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("molecules").join(mol_id);
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    // Legacy flat layout.
    let legacy = state_dir.join("molecules").join(mol_id);
    if legacy.is_dir() {
        return Some(legacy);
    }
    None
}

/// Pretty-print a markdown file to the terminal.
///
/// We deliberately do not pull in a full markdown renderer (termimad,
/// tui-markdown) for this path — keeping the dependency surface small is
/// worth more than perfect styling. A section header + raw body is
/// enough to demonstrate the "wow".
fn print_markdown_file(path: &Path, label: &str) -> anyhow::Result<()> {
    let body = std::fs::read_to_string(path)?;
    println!();
    println!(
        "{} {}",
        "📂".bold(),
        format!("{label} — {}", path.display()).dimmed(),
    );
    println!("{}", "─".repeat(72).dimmed());
    println!("{body}");
    println!("{}", "─".repeat(72).dimmed());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_question_mark() {
        assert_eq!(classify_prompt("Is X a Y?"), "deep-think");
        assert_eq!(classify_prompt("what about Z"), "deep-think");
        assert_eq!(classify_prompt("Comment faire X"), "deep-think");
    }

    #[test]
    fn classify_imperative() {
        assert_eq!(classify_prompt("Implement cs demo"), "task-work");
        assert_eq!(classify_prompt("Add a test"), "task-work");
        assert_eq!(classify_prompt("Refactor this module"), "task-work");
    }

    #[test]
    fn classify_default_is_idea() {
        assert_eq!(
            classify_prompt("We should think about better onboarding"),
            "idea-to-plan"
        );
    }

    #[test]
    fn var_key_by_formula() {
        assert_eq!(formula_variable_key("deep-think"), "question");
        assert_eq!(formula_variable_key("task-work"), "topic");
        assert_eq!(formula_variable_key("idea-to-plan"), "topic");
    }

    #[test]
    fn empty_prompt_rejected() {
        let args = Args {
            prompt: Some("   ".to_owned()),
            ..Default::default()
        };
        let err = resolve_prompt(&args).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn tackle_args_omit_adapter_by_default() {
        // Backward-compat: a demo invocation without `--adapter` produces
        // the historical `cs tackle <mol>` argv shape, so the
        // [adapters.default] / built-in resolution path inside tackle
        // remains the canonical fallback.
        let argv = build_tackle_args("task-20260519-dd69", None);
        assert_eq!(argv, vec!["tackle", "task-20260519-dd69"]);
    }

    #[test]
    fn tackle_args_forward_adapter_canonical_name() {
        // ADR-106 D4 — `llama-cpp` is the canonical name; `cs demo` threads
        // it verbatim into `cs tackle` so the AdapterSelected event records
        // the canonical form (not the alias).
        let argv = build_tackle_args("task-20260519-dd69", Some("llama-cpp"));
        assert_eq!(
            argv,
            vec!["tackle", "task-20260519-dd69", "--adapter", "llama-cpp"]
        );
    }

    #[test]
    fn tackle_args_forward_adapter_legacy_alias_unchanged() {
        // ADR-106 — canonicalisation lives in `cs tackle`, not in `cs demo`.
        // The orchestrator forwards `llama` verbatim and lets the validator
        // map it to `llama-cpp` so both invocation surfaces share one path.
        let argv = build_tackle_args("task-20260519-dd69", Some("llama"));
        assert_eq!(
            argv,
            vec!["tackle", "task-20260519-dd69", "--adapter", "llama"]
        );
    }
}
