// SPDX-License-Identifier: AGPL-3.0-only

//! The interactive `read → step → render` loop — the foreground driver.
//!
//! ## The REPL owns the loop (ADR-115, delib §4 Q1)
//!
//! claw-code's `ConversationRuntime::run_turn` and cosmon's chosen shape
//! agree: the **driver** owns the loop and calls a per-round `step()` on
//! the session, rather than the session owning a `for turn in 0..K` loop
//! with no exit point for operator input. [`run_repl`] is that driver.
//! Each iteration:
//!
//! 1. renders the `❯` prompt and reads one operator line;
//! 2. if the line is a [`crate::PilotDirective`], dispatches it **without
//!    touching the model** and returns to the prompt;
//! 3. otherwise submits the line as a turn and drives
//!    [`cosmon_agent_harness::InteractiveSession::step`] until the model
//!    yields (a `Turn::Stop` → [`StepOutcome::Yielded`]) or the per-turn
//!    budget is spent;
//! 4. appends the new transcript tail to disk and loops.
//!
//! EOF on the input (Ctrl-D, or a scripted reader running dry) ends the
//! loop exactly as `/quit` does — the FSM terminal `stopped` state is
//! always the *caller's* prerogative, never something `step()` can reach
//! on its own (the `InteractiveStopYields` invariant).

use std::io::{BufRead, Write};
use std::path::Path;

use cosmon_agent_harness::spine::Provider;
use cosmon_agent_harness::{
    InteractiveSession, StepOutcome, TerminalDisposition, Tool, ToolRegistry,
};

use crate::directives::PilotDirective;
use crate::error::PilotError;
use crate::transcript::Transcript;

/// The non-provider knobs a [`run_repl`] needs: the opening briefing the
/// session is seeded with, the `work_dir` tool calls execute against, and
/// the `observe` tool the `/observe` directive dispatches through.
///
/// The provider, tool registry, transcript, and I/O streams are passed
/// separately because they are owned (provider, transcript) or borrowed
/// mutably (I/O) — bundling only the plain borrows here keeps
/// [`run_repl`]'s argument count honest.
///
/// `Copy` (every field is a shared reference) but not `Debug` — `dyn Tool`
/// has no `Debug` bound.
#[derive(Clone, Copy)]
pub struct ReplConfig<'a> {
    /// The system framing / first context the session opens with — the
    /// pilot persona, constraints, and the fact that it can observe the
    /// fleet. Folded into the log as the seed turn.
    pub briefing: &'a str,
    /// The directory tool calls resolve against — both the harness
    /// filesystem tools and the cosmon-ops tools' walk-up to
    /// `.cosmon/state/`. Normally the operator's current galaxy root.
    /// Ignored by the remote backend (which resolves state over the wire).
    pub work_dir: &'a Path,
    /// The `observe` tool the `/observe` directive dispatches through —
    /// *the same backend the model uses*. Local sessions pass
    /// [`cosmon_ops_tools::ObserveTool`]; remote sessions pass
    /// [`cosmon_ops_tools::RemoteObserveTool`], so `/observe` honours the
    /// active backend instead of always reading the local store.
    pub observe: &'a dyn Tool,
}

/// Run the interactive pilot loop to completion.
///
/// Constructs a fresh [`InteractiveSession`] over `provider` with the
/// supplied read-only `registry` (the model may call those tools mid-turn),
/// then drives the read→step→render loop reading lines from `input` and
/// rendering to `output`, appending each turn's new entries to
/// `transcript`. Returns when the operator types `/quit` or `input` reaches
/// EOF.
///
/// # Errors
///
/// - [`PilotError::Harness`] if the session cannot be constructed
///   (context overflow on the briefing) or a `step()` fails fatally
///   (context overflow mid-loop, tool-budget exhaustion, provider death).
/// - [`PilotError::Io`] if rendering to `output`, reading `input`, or
///   appending the `transcript` fails.
pub async fn run_repl<P, In, Out>(
    provider: P,
    registry: ToolRegistry,
    config: ReplConfig<'_>,
    transcript: &mut Transcript,
    input: In,
    output: &mut Out,
) -> Result<(), PilotError>
where
    P: Provider,
    In: BufRead,
    Out: Write,
{
    let mut session =
        InteractiveSession::with_registry(provider, config.briefing, config.work_dir, registry)
            .map_err(|e| PilotError::Harness(e.to_string()))?;

    writeln!(
        output,
        "cosmon-pilot — local cognitive pilot. /help for directives, /quit to leave."
    )?;

    let mut input = input;
    let mut line = String::new();
    loop {
        write!(output, "❯ ")?;
        output.flush()?;

        line.clear();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            // EOF (Ctrl-D, or a scripted reader running dry) — leave the
            // loop the same way `/quit` does.
            writeln!(output)?;
            break;
        }
        let typed = line.trim();
        if typed.is_empty() {
            continue;
        }

        if let Some(directive) = PilotDirective::parse(typed) {
            if dispatch_directive(&directive, &mut session, config, output)? {
                break;
            }
            continue;
        }

        // A normal operator turn: submit and drive the model until it
        // yields control back to the prompt.
        session.submit(typed);
        loop {
            match session
                .step()
                .await
                .map_err(|e| PilotError::Harness(e.to_string()))?
            {
                StepOutcome::Continued => {}
                StepOutcome::Yielded(text) => {
                    writeln!(output, "{text}")?;
                    break;
                }
                StepOutcome::YieldedTerminal(terminal) => {
                    writeln!(output, "{}", terminal.text)?;
                    if terminal.disposition != TerminalDisposition::Normal {
                        writeln!(
                            output,
                            "(response ended: {})",
                            terminal_disposition_label(&terminal.disposition)
                        )?;
                    }
                    break;
                }
                StepOutcome::BudgetExhausted { limit } => {
                    writeln!(
                        output,
                        "(per-turn budget of {limit} round-trips spent — type to continue)"
                    )?;
                    break;
                }
                // `StepOutcome` is `#[non_exhaustive]`; an unforeseen
                // future outcome yields control to the operator rather than
                // looping forever.
                _ => break,
            }
        }

        transcript.append_new(&session.transcript())?;
    }

    // Flush any tail the last turn produced (covers a `/quit` issued right
    // after a model turn whose entries were already flushed — cheap no-op).
    transcript.append_new(&session.transcript())?;
    Ok(())
}

fn terminal_disposition_label(disposition: &TerminalDisposition) -> &str {
    match disposition {
        TerminalDisposition::Normal => "normal",
        TerminalDisposition::OutputLimit => "output limit",
        TerminalDisposition::Refused => "refused",
        TerminalDisposition::Incomplete => "incomplete response",
        TerminalDisposition::Unknown(_) => "unknown provider reason",
    }
}

/// Dispatch one [`PilotDirective`]. Returns `Ok(true)` when the directive
/// asks the loop to stop (`/quit`), `Ok(false)` otherwise.
///
/// Directives never reach the model — they mutate the session or print to
/// the operator and return straight to the prompt. The `/observe` directive
/// reuses [`ReplConfig::observe`] directly (the same backend tool the model
/// could call) so the operator inspects a molecule without spending a turn —
/// and so a remote session's `/observe` hits the wire, not the local store.
fn dispatch_directive<P, Out>(
    directive: &PilotDirective,
    session: &mut InteractiveSession<P>,
    config: ReplConfig<'_>,
    output: &mut Out,
) -> Result<bool, PilotError>
where
    P: Provider,
    Out: Write,
{
    match directive {
        PilotDirective::Quit => {
            writeln!(output, "leaving the pilot — transcript kept on disk.")?;
            return Ok(true);
        }
        PilotDirective::Help => {
            writeln!(output, "{}", PilotDirective::help_text())?;
        }
        PilotDirective::Compact => match session.compact() {
            Ok(report) => writeln!(
                output,
                "compacted: {} → {} tokens ({} messages removed).",
                report.tokens_before, report.tokens_after, report.messages_removed
            )?,
            Err(e) => writeln!(output, "compaction skipped: {e}.")?,
        },
        PilotDirective::Observe { molecule_id } => match molecule_id {
            None => writeln!(output, "usage: /observe <molecule-id>")?,
            Some(id) => {
                let args = serde_json::json!({ "molecule_id": id }).to_string();
                match config.observe.execute(&args, config.work_dir) {
                    Ok(json) => writeln!(output, "{json}")?,
                    Err(e) => writeln!(output, "observe failed: {e}")?,
                }
            }
        },
        PilotDirective::Unknown { verb } => {
            writeln!(output, "unknown directive: /{verb} — try /help")?;
        }
    }
    Ok(false)
}
