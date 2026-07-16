// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor whisper <molecule>` — whisper-channel scaffold probe.
//!
//! Non-invasive scaffold. Captures the current tmux pane state for the
//! worker assigned to the given molecule, classifies it, records
//! Claude-CLI and tmux versions, and writes an `experiment-report.md`
//! into the molecule directory enumerating the four design probes
//! (A/B/C/D) with predicted outcomes given the observed state. Live
//! invasive probes (actually sending whisper payloads and checking
//! queue/submit/refuse/rate-limit behavior) require the `cs whisper`
//! implementation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::id::MoleculeId;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeFilter, StateStore};

use super::Context;

/// Arguments for `cs doctor whisper`.
#[derive(clap::Args)]
pub struct WhisperArgs {
    /// Molecule ID (full or unambiguous prefix).
    pub molecule_id: String,
}

/// Observed state of a tmux pane (heuristic classification).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneState {
    ToolUse,
    ReplInput,
    ShellPrompt,
    NoSession,
    Unknown,
}

impl PaneState {
    fn as_str(self) -> &'static str {
        match self {
            Self::ToolUse => "tool-use",
            Self::ReplInput => "repl-input",
            Self::ShellPrompt => "shell-prompt",
            Self::NoSession => "no-session",
            Self::Unknown => "unknown",
        }
    }
}

/// Execute `cs doctor whisper`.
///
/// # Errors
/// Propagates errors from molecule resolution or report persistence.
pub fn run(ctx: &Context, args: &WhisperArgs) -> anyhow::Result<()> {
    let state_dir = ctx
        .config
        .clone()
        .unwrap_or_else(crate::cmd::default_state_dir);
    let store = FileStore::new(&state_dir);

    let mol = resolve_molecule(&store, &args.molecule_id)?;
    let mol_id = mol.id.clone();

    let socket = crate::cmd::tmux_socket_name(ctx);
    let worker_name = mol.assigned_worker.as_ref().map(|w| w.name().to_owned());
    let session_name = worker_name.clone();

    let capture = session_name
        .as_ref()
        .and_then(|s| capture_pane(&socket, s).ok());
    let state = capture
        .as_ref()
        .map_or(PaneState::NoSession, |c| classify_pane(c));

    let claude_version = run_capture("claude", &["--version"]).unwrap_or_else(|| "unknown".into());
    let tmux_version = run_capture("tmux", &["-V"]).unwrap_or_else(|| "unknown".into());

    let ts = chrono::Utc::now();
    let probe_log_dir = PathBuf::from(format!(
        "/tmp/cosmon-whisper-probe-{}",
        ts.format("%Y%m%dT%H%M%S")
    ));
    fs::create_dir_all(&probe_log_dir).ok();
    if let Some(cap) = &capture {
        let _ = fs::write(probe_log_dir.join("pane-capture.txt"), cap);
    }

    let report = render_report(
        &mol_id,
        worker_name.as_deref(),
        session_name.as_deref(),
        state,
        capture.as_deref(),
        &claude_version,
        &tmux_version,
        &ts,
        &probe_log_dir,
    );

    let mol_dir = store.molecule_dir(&mol_id);
    fs::create_dir_all(&mol_dir)
        .map_err(|e| anyhow::anyhow!("failed to create molecule dir: {e}"))?;
    let report_path = mol_dir.join("experiment-report.md");
    fs::write(&report_path, &report)
        .map_err(|e| anyhow::anyhow!("failed to write experiment-report.md: {e}"))?;

    if ctx.json {
        let out = serde_json::json!({
            "command": "doctor whisper",
            "molecule": mol_id.as_str(),
            "worker": worker_name,
            "pane_state": state.as_str(),
            "report_path": report_path.to_string_lossy(),
            "probe_log_dir": probe_log_dir.to_string_lossy(),
            "claude_version": claude_version.trim(),
            "tmux_version": tmux_version.trim(),
        });
        println!("{out}");
    } else {
        println!("doctor: whisper probe — molecule {}", mol_id.as_str());
        println!("  worker:      {}", worker_name.as_deref().unwrap_or("—"));
        println!("  pane state:  {}", state.as_str());
        println!("  claude:      {}", claude_version.trim());
        println!("  tmux:        {}", tmux_version.trim());
        println!("  report:      {}", report_path.display());
        println!("  probe logs:  {}", probe_log_dir.display());
    }
    Ok(())
}

fn resolve_molecule(
    store: &FileStore,
    id_or_prefix: &str,
) -> anyhow::Result<cosmon_state::MoleculeData> {
    if let Ok(exact) = MoleculeId::new(id_or_prefix) {
        if let Ok(m) = store.load_molecule(&exact) {
            return Ok(m);
        }
    }
    let all = store.list_molecules(&MoleculeFilter::default())?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|m| m.id.as_str().starts_with(id_or_prefix))
        .collect();
    match matches.len() {
        0 => Err(anyhow::anyhow!("no molecule matching \"{id_or_prefix}\"")),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => {
            let ids: Vec<_> = matches.iter().map(|m| m.id.as_str().to_owned()).collect();
            Err(anyhow::anyhow!(
                "ambiguous prefix \"{id_or_prefix}\" matches {n} molecules: {}",
                ids.join(", ")
            ))
        }
    }
}

fn capture_pane(socket: &str, session: &str) -> std::io::Result<String> {
    let out = Command::new("tmux")
        .args(["-L", socket, "capture-pane", "-t", session, "-p", "-S", "-"])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Classify a pane capture into a `PaneState` via a small set of heuristics.
fn classify_pane(capture: &str) -> PaneState {
    let tail: Vec<&str> = capture.lines().rev().take(30).collect();
    let tail_text = tail.iter().rev().copied().collect::<Vec<_>>().join("\n");

    if tail_text.contains("esc to interrupt")
        || tail_text.contains("⎿")
        || tail_text.contains("tool_use")
    {
        return PaneState::ToolUse;
    }
    if tail_text.contains("╭─") && tail_text.contains("│ >") {
        return PaneState::ReplInput;
    }
    if let Some(last) = capture.lines().rfind(|l| !l.trim().is_empty()) {
        let t = last.trim_end();
        if t.ends_with('$') || t.ends_with('%') || t.ends_with('#') || t.ends_with("$ ") {
            return PaneState::ShellPrompt;
        }
    }
    PaneState::Unknown
}

fn run_capture(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[allow(clippy::too_many_arguments)]
fn render_report(
    mol_id: &MoleculeId,
    worker: Option<&str>,
    session: Option<&str>,
    state: PaneState,
    capture: Option<&str>,
    claude_version: &str,
    tmux_version: &str,
    ts: &chrono::DateTime<chrono::Utc>,
    probe_log_dir: &Path,
) -> String {
    let (probe_a, probe_b, probe_c, probe_d) = predicted_outcomes(state);
    let capture_block = capture.map_or_else(
        || "_(no tmux session found for assigned worker)_".to_owned(),
        |c| {
            let tail: Vec<&str> = c.lines().rev().take(30).collect();
            let tail = tail.iter().rev().copied().collect::<Vec<_>>().join("\n");
            format!("```\n{tail}\n```")
        },
    );

    format!(
        r#"---
title: Whisper channel experiment report
molecule: {mol}
formula: cs doctor whisper
generated_at: {ts}
scaffold: true
---

# Whisper channel experiment — {mol}

Non-invasive scaffold probe written by `cs doctor whisper`. The four probes
below are the specification from `delib-20260414-b8e2`. Live invasive runs
require the `cs whisper` implementation from `task-20260414-7631`; until
then, this report documents the **observed pane state** and the
**predicted outcome** of each probe given that state.

## Environment

| Field | Value |
|-------|-------|
| Claude CLI version | `{claude_version}` |
| tmux version | `{tmux_version}` |
| Assigned worker | `{worker}` |
| tmux session | `{session}` |
| Observed pane state | `{state}` |
| Timestamp (UTC) | `{ts}` |
| Probe log dir | `{probe_log}` |

### Tail of current pane capture

{capture_block}

## Probes

### Probe A — worker in tool-use

> Setup: worker currently in tool-use (bash sleeping, LLM waiting).
> Expected: message queued ("Press up to edit queued messages"), integrated
> when tool returns.

**Predicted from observed state (`{state}`):** {probe_a}

### Probe B — worker at REPL input wait

> Setup: worker at REPL input wait (after last response).
> Expected: paste + 2×Enter submits the whisper as a user prompt immediately.

**Predicted from observed state (`{state}`):** {probe_b}

### Probe C — pane at shell prompt

> Setup: pane at shell prompt (worker crashed or `cs complete`d).
> Expected: **MUST REFUSE** with `SessionMismatch`; do not paste.

**Predicted from observed state (`{state}`):** {probe_c}

### Probe D — rapid burst (5 in 1 s)

> Setup: 5 rapid whispers within 1 s.
> Expected: first accepted, remainder rate-limited.

**Predicted from observed state (`{state}`):** {probe_d}

## Gating verdict

This report is a **scaffold**. It satisfies the structural gate
(report exists, versions captured, pane classified) but does **not**
yet satisfy the behavioural gate from the briefing.

<!-- Written by cs doctor whisper. Scaffold per task-20260414-cea5. -->
"#,
        mol = mol_id.as_str(),
        worker = worker.unwrap_or("—"),
        session = session.unwrap_or("—"),
        state = state.as_str(),
        ts = ts.to_rfc3339(),
        claude_version = claude_version.trim(),
        tmux_version = tmux_version.trim(),
        probe_log = probe_log_dir.display(),
        capture_block = capture_block,
        probe_a = probe_a,
        probe_b = probe_b,
        probe_c = probe_c,
        probe_d = probe_d,
    )
}

fn predicted_outcomes(
    state: PaneState,
) -> (&'static str, &'static str, &'static str, &'static str) {
    match state {
        PaneState::ToolUse => (
            "✅ matches setup — expect queued message",
            "N/A (worker is mid-tool-use, not at REPL input)",
            "N/A (pane is not at shell prompt)",
            "First queued; remainder should be rate-limited",
        ),
        PaneState::ReplInput => (
            "N/A (worker is idle at REPL, not in tool-use)",
            "✅ matches setup — expect immediate submit",
            "N/A (pane is not at shell prompt)",
            "First submit; remainder rate-limited",
        ),
        PaneState::ShellPrompt => (
            "N/A (no Claude process active)",
            "N/A (no Claude process active)",
            "✅ matches setup — MUST refuse with SessionMismatch",
            "All should refuse with SessionMismatch",
        ),
        PaneState::NoSession => (
            "N/A (no tmux session — nothing to probe)",
            "N/A (no tmux session — nothing to probe)",
            "N/A (no tmux session — nothing to probe)",
            "N/A (no tmux session — nothing to probe)",
        ),
        PaneState::Unknown => (
            "Indeterminate — inspect raw capture",
            "Indeterminate — inspect raw capture",
            "Indeterminate — inspect raw capture",
            "Indeterminate — inspect raw capture",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_shell_prompt_percent() {
        let cap = "some output\nlast line\nuser@host ~ % ";
        assert_eq!(classify_pane(cap), PaneState::ShellPrompt);
    }

    #[test]
    fn classify_shell_prompt_dollar() {
        let cap = "nothing\n$";
        assert_eq!(classify_pane(cap), PaneState::ShellPrompt);
    }

    #[test]
    fn classify_tool_use_banner() {
        let cap = "doing things\n  ⎿ Running bash command (esc to interrupt)\n";
        assert_eq!(classify_pane(cap), PaneState::ToolUse);
    }

    #[test]
    fn classify_repl_input() {
        let cap = "╭─────────────────╮\n│ >               │\n╰─────────────────╯\n";
        assert_eq!(classify_pane(cap), PaneState::ReplInput);
    }

    #[test]
    fn classify_unknown_blank() {
        assert_eq!(classify_pane(""), PaneState::Unknown);
    }

    #[test]
    fn predicted_outcomes_shell_prompt_must_refuse_on_c() {
        let (_, _, c, _) = predicted_outcomes(PaneState::ShellPrompt);
        assert!(c.contains("MUST refuse"));
    }
}
