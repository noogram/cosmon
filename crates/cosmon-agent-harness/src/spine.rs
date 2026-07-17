// SPDX-License-Identifier: AGPL-3.0-only

//! Agent-loop spine — the provider-agnostic FSM that drives a single
//! worker session through the [`Provider`] trait to a final
//! synthesis string.
//!
//! ADR-102 §D-3 (*"draw the spine NOW, but only the spine"*): the
//! eight FSM states from knuth §3 live in *control flow* — the
//! `for _turn in 0..K` loop and the `match Turn { ... }` arm — not
//! yet in the type system. The PR-A.5 follow-up promotes the
//! transitions to a typestate-encoded `Harness<S: HarnessState>`
//! (named in ADR-102 §D-6 for institutional memory; **not**
//! nucleated by this PR).
//!
//! ## Termination
//!
//! For any finite [`crate::budget::TurnBudget::max_turns`] `K`,
//! finite tool-execution time, and a provider response that respects
//! the OpenAI / Anthropic envelope, this function terminates after
//! at most `O(K)` provider round-trips. Knuth §6's proof:
//! `V = (K − turn, J − used_tools)` is a strictly decreasing
//! lexicographic variant, bounded below by `(0, 0)`. The
//! `provider.timeout` configured inside each [`Provider::one_turn`]
//! impl is a load-bearing termination witness — without it,
//! `Sending → Decoding` could block forever.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use cosmon_transport::spawn::AdapterTelemetry;

use crate::bootstrap;
use crate::budget::{ContextBudget, ToolBudget, TurnBudget};
use crate::compaction::{CompactionError, CompactionPolicy, CompactionReport};
use crate::error::HarnessError;
use crate::message_log::{MessageLog, TranscriptEntry};
use crate::tool::{
    default_registry, default_registry_with_operator_block, ToolCall, ToolDeclaration, ToolRegistry,
};

/// One round-trip outcome from [`Provider::one_turn`].
///
/// `L::AssistantMsg` is opaque to the spine — the provider returns
/// its native envelope and the spine hands it back through
/// [`MessageLog::append_assistant`] before dispatching any tool
/// calls. This is the I4-preserving discipline that
/// `cosmon-provider::openai`'s pre-extraction loop maintained by
/// `messages.push(choice.message)` before the for-loop over calls.
#[non_exhaustive]
pub enum Turn<L: MessageLog> {
    /// The model emitted one or more tool calls. The spine appends
    /// the assistant message to the log, dispatches each call
    /// through the registry, and appends the tool results before
    /// the next turn.
    ToolCalls {
        /// Provider-native assistant envelope, including the
        /// tool_calls themselves. Spine pushes this through
        /// [`MessageLog::append_assistant`] before any
        /// `append_tool_result` calls.
        assistant: L::AssistantMsg,
        /// Spine-internal representation of the model-emitted calls,
        /// translated by the provider's `one_turn` impl from its
        /// native envelope.
        calls: Vec<ToolCall>,
    },
    /// The model emitted a final text response. Spine returns it
    /// verbatim. The provider is responsible for treating any
    /// non-stop `finish_reason` (length, content_filter, …) as a
    /// loud terminator — translate to `Stop` rather than retrying
    /// silently.
    Stop(String),
}

/// What the spine asks every provider to do — two methods.
///
/// `Send + Sync` is required because the spine drives the loop with
/// `async` calls; `Self::Error` carries the provider's typed error
/// surface verbatim (avoiding the lossy `Box<dyn Error>` round-trip
/// tolnay §Q1 rejected).
///
/// ADR-102 §1 *Schema* concept lives entirely inside the impl of
/// this trait — the wire envelope, the serde types, the
/// `chat/completions` URL construction are all per-provider.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Per-provider message log carrying I4 (well-formedness).
    type Log: MessageLog;

    /// Provider-typed error. Surfaced through
    /// [`HarnessError::Provider`] without a downcast.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Execute one round-trip against the provider, given the
    /// current state of the [`Self::Log`]. Returns a [`Turn`]
    /// describing what the model said.
    ///
    /// # Errors
    ///
    /// Returns the provider's native error (transport, decode,
    /// rate-limit, …) on any HTTP-layer or schema-layer failure.
    async fn one_turn(&self, log: &Self::Log) -> Result<Turn<Self::Log>, Self::Error>;

    /// Tool declarations the spine wants advertised to the model on
    /// the next request. Iteration order matches
    /// [`crate::tool::ToolRegistry::declarations`] — `BTreeMap` key
    /// order (S5 stability).
    fn tool_schema(&self) -> Vec<ToolDeclaration>;
}

/// Boxed FnMut alias used by [`ScriptedProviderFn`] — extracted to
/// quiet `clippy::type_complexity` and to give the boxed closure a
/// name for documentation purposes.
type BoxedTurnFn<L, E> = Box<dyn FnMut(&L) -> Result<Turn<L>, E> + Send>;

/// Log-inspecting test double for the [`Provider`] trait.
///
/// The simplest [`Provider`] test doubles pop turns off a fixed
/// `Vec<Turn>` and **ignore** the [`MessageLog`] argument to
/// [`Provider::one_turn`]. That shape covers loop-bookkeeping tests
/// (turn budget, tool budget, stop semantics) but cannot catch a
/// re-priming bug — a regression where the spine forgets to append a
/// tool result, appends it at the wrong position, or re-emits the
/// briefing before the next turn, all of which break I4 invisibly
/// from the loop's perspective.
///
/// `ScriptedProviderFn<L, E>` takes a closure
/// `FnMut(&L) -> Result<Turn<L>, E>` instead. The closure receives
/// the *current* state of the log on every call, so a test can:
///
/// 1. Branch on log shape (turn count, last role, last content) when
///    deciding which [`Turn`] to emit.
/// 2. Assert that after a [`Turn::ToolCalls`] turn, the matching
///    `tool_result` is appended in its provider-native envelope
///    **before** the next [`Provider::one_turn`] dispatch.
///
/// `FnMut` is wrapped in [`Mutex`] so the trait-object impl is
/// `Send + Sync` (the bound the harness spine requires). Multiple
/// in-flight `one_turn` calls would serialize on the mutex; the spine
/// is single-threaded per-session, so the mutex is uncontended in
/// practice.
///
/// # Tool schema
///
/// Defaults to [`default_registry`]`().declarations()` so a test can
/// dispatch any tool in the v0 registry without wiring. Override with
/// [`ScriptedProviderFn::with_tool_schema`] when pinning a specific
/// subset.
///
/// # Origin
///
/// Mirrors the `ScriptedApiClient` pattern in claudecode's
/// `runtime::conversation` test harness — log-inspecting test
/// doubles are a generic, IP-free idea; the implementation here is
/// independent and provider-agnostic.
///
/// # Example
///
/// ```no_run
/// use std::sync::atomic::{AtomicUsize, Ordering};
/// use std::sync::Arc;
///
/// use cosmon_agent_harness::spine::{ScriptedProviderFn, Turn};
/// use cosmon_agent_harness::MessageLog;
///
/// # async fn run<L>() where L: MessageLog<AssistantMsg = String> + 'static {
/// let calls = Arc::new(AtomicUsize::new(0));
/// let calls_in = Arc::clone(&calls);
/// let provider = ScriptedProviderFn::<L, std::io::Error>::new(move |_log: &L| {
///     let n = calls_in.fetch_add(1, Ordering::SeqCst);
///     Ok::<_, std::io::Error>(Turn::Stop(format!("turn {n}")))
/// });
/// # let _ = provider;
/// # }
/// ```
pub struct ScriptedProviderFn<L, E>
where
    L: MessageLog,
    E: std::error::Error + Send + Sync + 'static,
{
    f: Mutex<BoxedTurnFn<L, E>>,
    tools: Vec<ToolDeclaration>,
}

impl<L, E> ScriptedProviderFn<L, E>
where
    L: MessageLog,
    E: std::error::Error + Send + Sync + 'static,
{
    /// Construct from a per-turn closure. The closure may capture
    /// shared state (`Arc<AtomicUsize>`, `Arc<Mutex<Vec<_>>>`, …) to
    /// thread a call counter or per-turn snapshot back to the test.
    pub fn new<F>(f: F) -> Self
    where
        F: FnMut(&L) -> Result<Turn<L>, E> + Send + 'static,
    {
        Self {
            f: Mutex::new(Box::new(f)),
            tools: default_registry().declarations(),
        }
    }

    /// Override the default tool schema. Use when the closure expects
    /// a specific subset of the v0 registry advertised to the model.
    #[must_use]
    pub fn with_tool_schema(mut self, tools: Vec<ToolDeclaration>) -> Self {
        self.tools = tools;
        self
    }
}

#[async_trait]
impl<L, E> Provider for ScriptedProviderFn<L, E>
where
    L: MessageLog,
    E: std::error::Error + Send + Sync + 'static,
{
    type Log = L;
    type Error = E;

    async fn one_turn(&self, log: &Self::Log) -> Result<Turn<Self::Log>, Self::Error> {
        let mut guard = self
            .f
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (guard)(log)
    }

    fn tool_schema(&self) -> Vec<ToolDeclaration> {
        self.tools.clone()
    }
}

/// Drive a single worker session from briefing to synthesis.
///
/// Behaviour-preserving extraction of the eight-turn loop body from
/// `cosmon-provider::openai::run_agent_loop`. The control flow is
/// identical to the pre-extraction baseline; the per-provider HTTP
/// envelope work has been pushed into [`Provider::one_turn`].
///
/// `_telemetry` is accepted for API compatibility with the
/// pre-extraction signature; the spine itself does not emit
/// telemetry in v0 (per-provider impls and the calling Adapter
/// already emit the ADR-100 silent-failure events from their
/// wrappers — see
/// `cosmon-provider::openai::run_agent_loop` for the mapping).
/// PR-A.5 may surface harness-level invariant breaches through this
/// channel.
///
/// # Errors
///
/// - [`HarnessError::ContextOverflow`] when the briefing's estimated
///   token count exceeds [`ContextBudget::DEFAULT`] (SF-5).
/// - [`HarnessError::Provider`] when [`Provider::one_turn`] fails.
///   The provider's typed error is surfaced verbatim.
/// - [`HarnessError::Tool`] when a dispatched tool returns an error
///   (bad arguments, refused path, IO failure).
/// - [`HarnessError::TurnBudgetExhausted`] when the model does not
///   return [`Turn::Stop`] within [`TurnBudget::DEFAULT`] turns.
/// - [`HarnessError::ToolBudgetExhausted`] when cumulative tool
///   dispatches exceed [`ToolBudget::DEFAULT`]. Closes the worst-case
///   30×64=1920-dispatch gap.
pub async fn run_loop<P: Provider>(
    provider: &P,
    briefing: &str,
    work_dir: &Path,
    telemetry: Option<&AdapterTelemetry>,
) -> Result<String, HarnessError<P::Error>> {
    run_loop_with_registry(provider, briefing, work_dir, telemetry, default_registry()).await
}

/// Drive a worker session with a fixed capability registry.
///
/// The caller selects the finite tool set before the first model turn, so an
/// omitted capability cannot be recovered by a later model response.
///
/// # Errors
///
/// Returns the same provider, context-budget, turn-budget, and tool-execution
/// errors as [`run_loop`].
pub async fn run_loop_with_registry<P: Provider>(
    provider: &P,
    briefing: &str,
    work_dir: &Path,
    telemetry: Option<&AdapterTelemetry>,
    registry: ToolRegistry,
) -> Result<String, HarnessError<P::Error>> {
    run_loop_with_registry_impl(provider, briefing, work_dir, telemetry, registry).await
}

/// Drive a worker session, gating the `await_operator` blocking primitive
/// on the molecule's operator-block capability (ADR-123).
///
/// This compatibility entry point retains the standard registry while the
/// caller-controlled [`run_loop_with_registry`] permits a stricter local
/// capability set.
///
/// # Errors
///
/// Returns the same provider, context-budget, turn-budget, and tool-execution
/// errors as [`run_loop`].
pub async fn run_loop_with_capability<P: Provider>(
    provider: &P,
    briefing: &str,
    work_dir: &Path,
    telemetry: Option<&AdapterTelemetry>,
    capability: Option<&cosmon_core::operator_block::OperatorBlockCapability>,
) -> Result<String, HarnessError<P::Error>> {
    run_loop_with_registry(
        provider,
        briefing,
        work_dir,
        telemetry,
        default_registry_with_operator_block(capability),
    )
    .await
}

/// Drive a worker session, gating the `await_operator` blocking primitive
/// on the molecule's operator-block capability (ADR-123).
///
/// Identical to [`run_loop`] except the tool registry is built with
/// [`default_registry_with_operator_block`]:
///
/// - `capability` **`None`** ⇒ the seven base tools, no blocking
///   primitive — a worker that wants to pause has no way to do so through
///   the harness and must surface-and-continue. (This is what [`run_loop`]
///   passes, so the default path is unchanged.)
/// - `capability` **`Some(..)`** ⇒ the base tools **plus** `await_operator`,
///   the single sanctioned, signal-emitting blocking primitive.
///
/// This closes the asymmetry the task names: until now an `awaiting_operator`
/// affordance lived only on the REPL [`InteractiveSession`], never on the
/// one-shot worker loop. Now the worker loop, too, can pause for an
/// operator — but **only** observably, and **only** when the typed
/// capability authorises it. No modal tool is ever registered, so an
/// invisible off-cosmon block is unavailable by construction (the original
/// incident).
///
/// # Errors
///
/// Identical to [`run_loop`].
async fn run_loop_with_registry_impl<P: Provider>(
    provider: &P,
    briefing: &str,
    work_dir: &Path,
    _telemetry: Option<&AdapterTelemetry>,
    registry: ToolRegistry,
) -> Result<String, HarnessError<P::Error>> {
    // Bootstrapping (knuth §7) — walk up from `work_dir` collecting
    // `AGENTS.md` / `CLAUDE.md`, prepend them to the briefing so the
    // model sees project conventions before its first tool call.
    // Always-prepend by design: there is no conditional injection.
    let bootstrap_prefix = bootstrap::collect_bootstrap_context(work_dir);
    let effective_briefing = if bootstrap_prefix.is_empty() {
        briefing.to_owned()
    } else {
        format!("{bootstrap_prefix}\n\n{briefing}")
    };

    // I3 pre-flight (SF-5) — 4-chars-per-token heuristic, matches the
    // pre-extraction `openai::MAX_INPUT_TOKENS` discipline. The check
    // sees the *augmented* briefing, so a 4 GiB `CLAUDE.md` blown
    // through pre-turn injection trips SF-5 loudly per knuth I3.
    let context_limit = ContextBudget::DEFAULT.max_input_tokens;
    let estimated = estimate_briefing_tokens(&effective_briefing);
    if estimated > context_limit {
        return Err(HarnessError::ContextOverflow {
            estimated_tokens: estimated,
            limit: context_limit,
        });
    }

    let mut log = <P::Log>::from_briefing(&effective_briefing);
    let max_turns = TurnBudget::DEFAULT.max_turns;
    let tool_limit = ToolBudget::DEFAULT.max_tool_calls;
    // I2 — cumulative tool dispatch counter (delib-20260519-e6db W5 /
    // knuth §K7). Lyapunov-variant `V = (K − turn, J − used_tools)`
    // requires both axes bound; without `used_tools` the worst-case
    // is 30×64=1920 dispatches before `TurnBudgetExhausted`.
    let mut used_tools: u32 = 0;

    // C4 mechanism 5 (delib-20260705-7288) — tool-call cycle detection.
    // A weak local oracle can loop *across turns* (turn: `read A`; turn:
    // `read A`; …) and burn the whole turn/tool budget without progress.
    // We fingerprint each *turn's* tool-call sequence as one token and scan
    // the tail for a repeating block. Turn granularity is deliberate: a
    // within-turn fan-out (65 identical calls in one turn) is one decision
    // bounded by the tool budget, not a stuck loop — only cross-turn
    // repetition of the same behaviour is a cycle. This guard can only make
    // the loop terminate *earlier* than the turn/tool budgets would, so the
    // spine's `O(K)` termination proof is preserved. Grammar-constrained
    // decoding + the budgets stay the load-bearing guarantees; the cycle
    // check is the tighter early signal.
    let mut turn_fingerprints: Vec<cosmon_core::oracle_boundary::ToolCallFingerprint> = Vec::new();

    // Compaction policy is currently hard-coded to the default. A
    // `task-20260521-d436` follow-up can lift it through
    // `[adapters.<name>].compaction.{threshold_ratio, target_ratio}`
    // in `Adapter.toml` once a second use-case asks for it; the
    // current default is sane for any provider with `n_ctx` ≥ 8_192.
    let context_budget = ContextBudget::DEFAULT;
    let compaction_policy = CompactionPolicy::DEFAULT;
    let compaction_threshold = compaction_policy.threshold_tokens(context_budget);
    let compaction_target = compaction_policy.target_tokens(context_budget);

    for _turn in 0..max_turns {
        // I4 self-check — release builds skip this; debug builds
        // catch a per-provider impl that violated well-formedness.
        debug_assert!(
            log.invariant_well_formed(),
            "MessageLog::invariant_well_formed returned false — I4 breach"
        );

        // Compaction trigger (task-20260521-d436). Shared with the
        // interactive `InteractiveSession::step` path so both callers
        // see byte-identical escape-valve semantics — see
        // [`maybe_compact`].
        maybe_compact(
            &mut log,
            compaction_threshold,
            compaction_target,
            compaction_policy,
        );

        // I3 in-loop enforcement (delib-20260519-e6db W5 / knuth §K6,
        // forgemaster AH5). The pre-flight check at the head of
        // `run_loop` only sees the augmented briefing; once tool
        // results start landing the log grows monotonically and the
        // bound must be re-checked every turn or context overflow
        // surfaces as opaque `HarnessError::Provider` instead of
        // the structurally correct `ContextOverflow`.
        let estimated_now = log.estimate_tokens();
        if estimated_now > context_limit {
            return Err(HarnessError::ContextOverflow {
                estimated_tokens: estimated_now,
                limit: context_limit,
            });
        }

        let turn = provider
            .one_turn(&log)
            .await
            .map_err(HarnessError::Provider)?;

        match turn {
            Turn::ToolCalls { assistant, calls } => {
                // C4 mechanism 5 — collapse this turn's calls into one
                // fingerprint *before* they are moved into dispatch, then
                // scan the tail for a stuck loop. The per-call `id` is
                // excluded (it varies per call); only `(name, arguments)`
                // per call decides whether two turns "do the same thing".
                let turn_fp = cosmon_core::oracle_boundary::ToolCallFingerprint::of(
                    "turn",
                    &calls
                        .iter()
                        .map(|c| format!("{}\u{1}{}", c.name, c.arguments_json))
                        .collect::<Vec<_>>()
                        .join("\u{2}"),
                );
                turn_fingerprints.push(turn_fp);
                if let Some(report) = cosmon_core::oracle_boundary::detect_tool_call_cycle(
                    &turn_fingerprints,
                    cosmon_core::oracle_boundary::DEFAULT_CYCLE_REPEATS,
                ) {
                    return Err(HarnessError::ToolCallCycle {
                        period: report.period,
                        repeats: report.repeats,
                    });
                }

                // Shared with `InteractiveSession::step` — the I4
                // ordering (assistant-before-results), the I2 budget,
                // and the recover-from-tool-error discipline all live
                // in [`dispatch_tool_calls`].
                dispatch_tool_calls(
                    &mut log,
                    &registry,
                    work_dir,
                    assistant,
                    calls,
                    &mut used_tools,
                    tool_limit,
                )
                .map_err(|DispatchHalt::ToolBudgetExhausted { limit }| {
                    HarnessError::ToolBudgetExhausted { limit }
                })?;
            }
            Turn::Stop(text) => return Ok(text),
        }
    }

    Err(HarnessError::TurnBudgetExhausted { limit: max_turns })
}

/// Non-recoverable halt reason from [`dispatch_tool_calls`].
///
/// A tool *execution* failure is never a halt — it is fed back to the
/// model as a tool result so it can recover next turn. The only thing
/// that stops dispatch mid-turn is exceeding the I2 tool budget; this
/// one-variant enum keeps the helper's error channel honest (and lets
/// both callers map it onto their own outcome type — a hard
/// [`HarnessError`] for the worker path, the same for the interactive
/// path since a runaway tool fan-out is fatal in both regimes).
enum DispatchHalt {
    /// Cumulative tool dispatches crossed the budget; the offending
    /// call's body was NOT run.
    ToolBudgetExhausted {
        /// The budget that was exceeded.
        limit: u32,
    },
}

/// Append the assistant envelope and dispatch its tool calls, appending
/// each result to the log. Shared verbatim by [`run_loop`] and
/// [`InteractiveSession::step`] so the two paths cannot drift on the
/// three disciplines that live here:
///
/// 1. **I4 ordering** — the assistant message lands *before* any tool
///    result, matching the pre-extraction `messages.push(choice.message)`
///    ordering of the original `openai.rs`.
/// 2. **I2 budget** — `used_tools` is incremented *before* dispatch, so
///    the 65th call in a single fan-out trips the budget without running
///    its body. Saturating add quiets clippy on the unreachable
///    `u32::MAX` wrap-around.
/// 3. **Recover, don't abort** — a tool execution failure (ENOENT, bad
///    path, invalid arguments) is an OBSERVATION the model must see and
///    correct, not a fatal condition. The typed error is fed back as the
///    tool result; the turn/tool budgets still bound any loop on a broken
///    tool.
fn dispatch_tool_calls<L: MessageLog>(
    log: &mut L,
    registry: &ToolRegistry,
    work_dir: &Path,
    assistant: L::AssistantMsg,
    calls: Vec<ToolCall>,
    used_tools: &mut u32,
    tool_limit: u32,
) -> Result<(), DispatchHalt> {
    log.append_assistant(assistant);
    for call in calls {
        *used_tools = used_tools.saturating_add(1);
        if *used_tools > tool_limit {
            return Err(DispatchHalt::ToolBudgetExhausted { limit: tool_limit });
        }
        let result = match registry.execute(&call, work_dir) {
            Ok(output) => output,
            Err(tool_err) => {
                tracing::debug!(
                    target = "cosmon_agent_harness::tool",
                    tool = %call.name,
                    error = %tool_err,
                    "tool execution failed; feeding error back to the model"
                );
                format!("ERROR: {tool_err}")
            }
        };
        log.append_tool_result(&call.id, &call.name, &result);
    }
    Ok(())
}

/// Run the per-turn compaction trigger. When the
/// log estimate crosses `threshold`, ask the per-provider
/// `MessageLog::compact` impl to reduce it toward `target`. Compaction
/// is an INFORMATIONAL escape valve — its failure modes
/// (`NotApplicable`, `WouldBreakInvariant`) are non-fatal; the caller
/// continues and the I3 check remains the loud gate. Successful
/// compaction emits a `tracing` breadcrumb without interrupting the hot
/// path. Shared by [`run_loop`] and [`InteractiveSession::step`].
fn maybe_compact<L: MessageLog>(
    log: &mut L,
    threshold: u32,
    target: u32,
    policy: CompactionPolicy,
) {
    let estimated_pre_compact = log.estimate_tokens();
    if estimated_pre_compact <= threshold {
        return;
    }
    match log.compact(target, policy) {
        Ok(report) => {
            tracing::debug!(
                target = "cosmon_agent_harness::compaction",
                tokens_before = report.tokens_before,
                tokens_after = report.tokens_after,
                messages_removed = report.messages_removed,
                "compacted MessageLog above threshold"
            );
        }
        Err(CompactionError::NotApplicable) => {
            tracing::trace!(
                target = "cosmon_agent_harness::compaction",
                tokens = estimated_pre_compact,
                "compaction not applicable; log too small to compact"
            );
        }
        Err(CompactionError::WouldBreakInvariant) => {
            tracing::debug!(
                target = "cosmon_agent_harness::compaction",
                tokens = estimated_pre_compact,
                "compaction refused; would break I4 — retry next turn"
            );
        }
    }
}

/// What a single [`InteractiveSession::step`] resolved to — the
/// caller-facing projection of the interactive-mode transitions in the
/// `cs_pilot_interactive_fsm` TLA+ spec (ADR-115).
///
/// The load-bearing property the spec calls `InteractiveStopYields` is
/// encoded *in the type*: there is **no** variant that terminates the
/// session. A model `Turn::Stop` resolves to [`StepOutcome::Yielded`]
/// (FSM `decoding → yield → awaiting`); a spent per-turn budget resolves
/// to [`StepOutcome::BudgetExhausted`] (FSM `BudgetExhausted → yield`).
/// Both hand control back to the caller, which decides whether to
/// [`InteractiveSession::submit`] another operator turn or stop the
/// REPL. Reaching the FSM's terminal `stopped` state is therefore the
/// *caller's* prerogative (the `/quit` directive), never something
/// `step()` can do on its own — which is exactly the invariant the spec
/// proves load-bearing via its dormant `SilentTerminate` action.
///
/// Contrast the worker path: [`run_loop`] returns
/// `Err(HarnessError::TurnBudgetExhausted)` on a spent budget, because a
/// one-shot worker has no operator to yield to (FSM `BudgetExhausted →
/// stopped` in `worker` mode).
#[non_exhaustive]
#[derive(Debug)]
pub enum StepOutcome {
    /// The model emitted tool calls; the session appended the assistant
    /// envelope and dispatched the calls, folding each result back into
    /// the log (FSM `decoding → dispatching → sending`). The caller
    /// should invoke [`InteractiveSession::step`] again to continue the
    /// current operator turn.
    Continued,
    /// The model emitted a final text response (FSM `decoding →
    /// yield`). The session hands control back to the operator: render
    /// this text and await the next [`InteractiveSession::submit`]. The
    /// session is **not** terminated.
    Yielded(String),
    /// The per-operator-turn round-trip budget
    /// ([`TurnBudget::max_turns`]) was spent and the model still wanted
    /// to continue (FSM `BudgetExhausted → yield`, interactive arm).
    /// Like [`StepOutcome::Yielded`] this hands control back to the
    /// operator rather than erroring — the model simply ran out of
    /// turns this round.
    BudgetExhausted {
        /// The per-turn budget that was reached.
        limit: u32,
    },
}

/// An interactive, caller-driven agent session — the `step()` entry the
/// `cs pilot` REPL drives turn-by-turn, the interactive counterpart to
/// the one-shot [`run_loop`].
///
/// # Why this exists (ADR-115, the bridge to ADR-102 §D-6)
///
/// [`run_loop`] owns its `for _turn in 0..K` loop and terminates hard on
/// `Turn::Stop`: a single immutable briefing in, a single synthesis out,
/// no exit point for operator input, the log write-only. That is exactly
/// right for an autonomous worker and exactly wrong for a REPL, where the
/// *operator* owns the loop and a model `Turn::Stop` must **yield** so
/// the human can type the next line. `InteractiveSession` inverts the
/// control: the caller owns the loop and calls [`Self::step`] for each
/// model round-trip, [`Self::submit`] for each operator turn, and reads
/// history back through [`Self::transcript`].
///
/// This is the runtime-FSM bridge toward the deferred typestate
/// `Harness<S: HarnessState>` named in ADR-102 §D-6: the eight states
/// still live in control flow (an `awaiting_operator` flag + the per-turn
/// counter), not yet in the type system. Promoting them to a typestate is
/// the PR-A.5 follow-up — this struct is the interim that proves the
/// transitions before they are encoded as types.
///
/// # No claw vocabulary (ADR-096)
///
/// `InteractiveSession`, `step`, `submit`, `transcript` — physics-neutral
/// names. The conversation artefact is a *transcript*; the in-REPL
/// meta-commands (a `/quit` etc.) are *pilot directives* owned by the
/// `cosmon-pilot` driver, not this crate.
///
/// # Worker path untouched
///
/// [`run_loop`] is unchanged in behaviour — it is the proven autonomy
/// path and shares only the *helpers* (`dispatch_tool_calls`,
/// `maybe_compact`) with this struct, never its control flow. The TLA+
/// `WorkerPathUnchanged` invariant is honoured by construction: the
/// worker never visits the `awaiting`/`yield` states because it never
/// constructs an `InteractiveSession`.
pub struct InteractiveSession<P: Provider> {
    /// The provider, owned for the session's lifetime. The REPL builds
    /// one provider per session, so ownership (not `&P`) is the
    /// ergonomic choice and keeps the borrow-checker out of the
    /// caller's loop.
    provider: P,
    /// The tool registry — the v0 default. A future pilot may inject a
    /// cosmon-ops registry here; v0 keeps it the same surface
    /// `run_loop` advertises.
    registry: ToolRegistry,
    /// The provider-shaped message log (I4 carrier). Opaque to the
    /// caller except through [`Self::transcript`].
    log: P::Log,
    /// Where tool calls execute. Stored as a `PathBuf` because the
    /// session outlives any single borrow.
    work_dir: PathBuf,
    /// FSM `awaiting` flag — `true` iff the session is quiescent at the
    /// `❯` prompt (no operator turn in flight). Set by [`Self::new`]
    /// (Init), cleared by [`Self::submit`] (OperatorSubmit), re-set when
    /// [`Self::step`] yields.
    awaiting_operator: bool,
    /// Model↔tool round-trips spent in the *current* operator turn.
    /// Reset to 0 by [`Self::submit`]; bounds the per-turn ping-pong
    /// (FSM `turn_count`, the `MaxTurns` axis).
    turn_count: u32,
    /// Cumulative tool dispatches across the whole session (I2). Unlike
    /// `turn_count` this is **not** reset per operator turn — a runaway
    /// fan-out is fatal regardless of how it is spread across turns.
    used_tools: u32,
    /// Per-turn round-trip cap (FSM `MaxTurns`).
    max_turns: u32,
    /// Cumulative tool-dispatch cap (I2).
    tool_limit: u32,
    /// Context-token ceiling (I3).
    context_limit: u32,
    /// Compaction policy + derived thresholds, computed once at
    /// construction (identical to `run_loop`'s locals).
    compaction_policy: CompactionPolicy,
    /// Token count above which [`maybe_compact`] fires.
    compaction_threshold: u32,
    /// Token count compaction aims to reduce the log toward.
    compaction_target: u32,
}

impl<P: Provider> InteractiveSession<P> {
    /// Open an interactive session from an initial briefing.
    ///
    /// Mirrors the head of [`run_loop`]: collects bootstrap context
    /// (`AGENTS.md` / `CLAUDE.md` walk-up), prepends it to the briefing,
    /// runs the I3 pre-flight check, and seeds the log via
    /// [`MessageLog::from_briefing`]. The session starts in the FSM
    /// `awaiting` state — the caller [`Self::submit`]s the first
    /// operator turn before the first [`Self::step`].
    ///
    /// The `briefing` is the system framing / first context the session
    /// opens with; subsequent operator lines arrive through
    /// [`Self::submit`].
    ///
    /// # Errors
    ///
    /// [`HarnessError::ContextOverflow`] when the bootstrap-augmented
    /// briefing already exceeds [`ContextBudget::DEFAULT`] (SF-5) — the
    /// same loud pre-flight gate `run_loop` enforces.
    pub fn new(
        provider: P,
        briefing: &str,
        work_dir: &Path,
    ) -> Result<Self, HarnessError<P::Error>> {
        Self::with_registry(provider, briefing, work_dir, default_registry())
    }

    /// Open an interactive session with a caller-supplied tool registry.
    ///
    /// Identical to [`Self::new`] in every respect except the tool
    /// registry advertised to the model. [`Self::new`] wires the
    /// [`default_registry`] (the filesystem trio + local-research
    /// extension); this variant lets a driver inject a *domain* registry
    /// instead — e.g. the cs-pilot REPL hands in the read-only
    /// `cosmon-ops-tools` registry so the model can `observe` / `peek` /
    /// `ensemble` the fleet during a turn. This is the injection point the
    /// struct docs anticipate ("a future pilot may inject a cosmon-ops
    /// registry here"); keeping it a separate constructor leaves the
    /// proven worker-path default untouched.
    ///
    /// # Errors
    ///
    /// [`HarnessError::ContextOverflow`] when the bootstrap-augmented
    /// briefing already exceeds [`ContextBudget::DEFAULT`] (SF-5) — the
    /// same loud pre-flight gate [`Self::new`] enforces.
    pub fn with_registry(
        provider: P,
        briefing: &str,
        work_dir: &Path,
        registry: ToolRegistry,
    ) -> Result<Self, HarnessError<P::Error>> {
        let bootstrap_prefix = bootstrap::collect_bootstrap_context(work_dir);
        let effective_briefing = if bootstrap_prefix.is_empty() {
            briefing.to_owned()
        } else {
            format!("{bootstrap_prefix}\n\n{briefing}")
        };

        let context_limit = ContextBudget::DEFAULT.max_input_tokens;
        let estimated = estimate_briefing_tokens(&effective_briefing);
        if estimated > context_limit {
            return Err(HarnessError::ContextOverflow {
                estimated_tokens: estimated,
                limit: context_limit,
            });
        }

        let context_budget = ContextBudget::DEFAULT;
        let compaction_policy = CompactionPolicy::DEFAULT;
        Ok(Self {
            provider,
            registry,
            log: <P::Log>::from_briefing(&effective_briefing),
            work_dir: work_dir.to_path_buf(),
            awaiting_operator: true,
            turn_count: 0,
            used_tools: 0,
            max_turns: TurnBudget::DEFAULT.max_turns,
            tool_limit: ToolBudget::DEFAULT.max_tool_calls,
            context_limit,
            compaction_policy,
            compaction_threshold: compaction_policy.threshold_tokens(context_budget),
            compaction_target: compaction_policy.target_tokens(context_budget),
        })
    }

    /// Submit an operator turn (FSM `OperatorSubmit`:
    /// `awaiting → sending`).
    ///
    /// Folds `operator_input` into the log as a `user` turn, resets the
    /// per-turn round-trip budget, and clears the `awaiting` flag so the
    /// next [`Self::step`] sends to the model. Call this once per line
    /// the operator types; then drive [`Self::step`] until it yields.
    pub fn submit(&mut self, operator_input: &str) {
        self.log.append_user(operator_input);
        self.turn_count = 0;
        self.awaiting_operator = false;
    }

    /// Drive one model round-trip (the interactive `step()` — FSM
    /// `sending → decoding → {dispatching → sending | yield}`).
    ///
    /// On [`Turn::ToolCalls`] the session appends the assistant envelope,
    /// dispatches the calls, folds the results back, and returns
    /// [`StepOutcome::Continued`] (the caller loops). On [`Turn::Stop`]
    /// it returns [`StepOutcome::Yielded`] and re-enters the `awaiting`
    /// state — it does **not** terminate. When the per-turn budget is
    /// spent it returns [`StepOutcome::BudgetExhausted`], also yielding.
    ///
    /// This is the one place the `InteractiveStopYields` invariant is
    /// realised in code: `step()` has no return path to a terminal
    /// `stopped`. The compaction trigger and I3 in-loop check are
    /// identical to [`run_loop`]'s (shared via `maybe_compact`).
    ///
    /// # Errors
    ///
    /// - [`HarnessError::ContextOverflow`] when the log grew past the I3
    ///   ceiling (same loud gate as the worker path).
    /// - [`HarnessError::Provider`] when [`Provider::one_turn`] fails.
    /// - [`HarnessError::ToolBudgetExhausted`] when cumulative tool
    ///   dispatches exceed [`ToolBudget::DEFAULT`] — a runaway fan-out is
    ///   fatal in the interactive regime too, unlike a spent *turn*
    ///   budget which merely yields.
    ///
    /// # Panics
    ///
    /// Never panics. (The `debug_assert!` I4 self-check is a no-op in
    /// release builds, identical to `run_loop`.)
    pub async fn step(&mut self) -> Result<StepOutcome, HarnessError<P::Error>> {
        // FSM `BudgetExhausted` (interactive arm): the per-turn budget
        // is spent and we have not yet seen a `Turn::Stop`. Yield to the
        // operator rather than erroring — the worker path errors here,
        // the interactive path hands control back.
        if self.turn_count >= self.max_turns {
            self.awaiting_operator = true;
            return Ok(StepOutcome::BudgetExhausted {
                limit: self.max_turns,
            });
        }

        // I4 self-check — release builds skip this; debug builds catch a
        // per-provider impl that violated well-formedness (same as
        // `run_loop`'s loop head).
        debug_assert!(
            self.log.invariant_well_formed(),
            "MessageLog::invariant_well_formed returned false — I4 breach"
        );

        // Compaction trigger — byte-identical semantics to `run_loop`.
        maybe_compact(
            &mut self.log,
            self.compaction_threshold,
            self.compaction_target,
            self.compaction_policy,
        );

        // I3 in-loop enforcement (delib-20260519-e6db W5 / knuth §K6).
        let estimated_now = self.log.estimate_tokens();
        if estimated_now > self.context_limit {
            return Err(HarnessError::ContextOverflow {
                estimated_tokens: estimated_now,
                limit: self.context_limit,
            });
        }

        // FSM `SendToModel` — one round-trip counts against the per-turn
        // budget.
        let turn = self
            .provider
            .one_turn(&self.log)
            .await
            .map_err(HarnessError::Provider)?;
        self.turn_count = self.turn_count.saturating_add(1);

        match turn {
            Turn::ToolCalls { assistant, calls } => {
                dispatch_tool_calls(
                    &mut self.log,
                    &self.registry,
                    &self.work_dir,
                    assistant,
                    calls,
                    &mut self.used_tools,
                    self.tool_limit,
                )
                .map_err(|DispatchHalt::ToolBudgetExhausted { limit }| {
                    HarnessError::ToolBudgetExhausted { limit }
                })?;
                Ok(StepOutcome::Continued)
            }
            // THE load-bearing branch (`InteractiveStopYields`): a model
            // `Turn::Stop` routes to `yield`, NOT to a terminal stop.
            Turn::Stop(text) => {
                self.awaiting_operator = true;
                Ok(StepOutcome::Yielded(text))
            }
        }
    }

    /// Read the conversation history back as a render-ready
    /// [`TranscriptEntry`] list — the accessor the REPL paints the
    /// scrollback from. Delegates to [`MessageLog::transcript`].
    #[must_use]
    pub fn transcript(&self) -> Vec<TranscriptEntry> {
        self.log.transcript()
    }

    /// `true` iff the session is quiescent at the `❯` prompt (FSM
    /// `awaiting` / `stopped`). The caller checks this to decide whether
    /// to prompt the operator or keep calling [`Self::step`].
    #[must_use]
    pub fn is_awaiting_operator(&self) -> bool {
        self.awaiting_operator
    }

    /// Model↔tool round-trips spent in the current operator turn. Reset
    /// by [`Self::submit`]. Exposed for the driver's status line and for
    /// tests asserting the per-turn budget.
    #[must_use]
    pub fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// Cumulative tool dispatches across the whole session (I2).
    #[must_use]
    pub fn used_tools(&self) -> u32 {
        self.used_tools
    }

    /// Force a compaction of the conversation log toward the session's
    /// configured target size — the primitive behind the `/compact` pilot
    /// directive.
    ///
    /// [`Self::step`] already compacts *automatically* once the log
    /// crosses the threshold (identical semantics to [`run_loop`] via
    /// `maybe_compact`). This method lets the operator request a
    /// compaction *now*, between turns, without waiting for the threshold
    /// — the manual counterpart to that automatic trigger. It calls the
    /// same [`MessageLog::compact`] the spine uses, so the I4
    /// well-formedness guarantee is preserved by the impl.
    ///
    /// # Errors
    ///
    /// - [`CompactionError::NotApplicable`] when the log is already at or
    ///   below the target (nothing to remove) — not a failure, just a
    ///   no-op the caller should report as "already compact".
    /// - [`CompactionError::WouldBreakInvariant`] when the log is
    ///   mid-tool-pair and compacting would orphan a tool result (I4) —
    ///   transient; succeeds once the turn completes.
    pub fn compact(&mut self) -> Result<CompactionReport, CompactionError> {
        self.log
            .compact(self.compaction_target, self.compaction_policy)
    }
}

/// 4-chars-per-token heuristic used by the I3 pre-flight check. Lives
/// as a free function so PR-A.5 can swap in a real tokenizer without
/// touching `run_loop`'s control flow.
fn estimate_briefing_tokens(briefing: &str) -> u32 {
    // Saturating cast keeps clippy happy for extreme inputs (a 4 GiB
    // briefing would overflow u32; we'd never reach that with a
    // 4_096-token ceiling).
    u32::try_from(briefing.len() / 4).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::{CompactionError, CompactionPolicy, CompactionReport};
    use crate::message_log::{MessageLog, TranscriptRole};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Minimal `MessageLog` impl for spine tests.
    struct TestLog {
        messages: Vec<String>,
    }

    impl MessageLog for TestLog {
        type AssistantMsg = String;

        fn from_briefing(briefing: &str) -> Self {
            Self {
                messages: vec![format!("user: {briefing}")],
            }
        }

        fn append_assistant(&mut self, msg: Self::AssistantMsg) {
            self.messages.push(format!("assistant: {msg}"));
        }

        fn append_tool_result(&mut self, call_id: &str, tool_name: &str, content: &str) {
            self.messages
                .push(format!("tool[{call_id}/{tool_name}]: {content}"));
        }

        fn append_user(&mut self, content: &str) {
            self.messages.push(format!("user: {content}"));
        }

        fn transcript(&self) -> Vec<TranscriptEntry> {
            self.messages.iter().map(|m| test_log_entry(m)).collect()
        }

        fn estimate_tokens(&self) -> u32 {
            u32::try_from(self.messages.iter().map(String::len).sum::<usize>() / 4)
                .unwrap_or(u32::MAX)
        }

        fn invariant_well_formed(&self) -> bool {
            true
        }
    }

    /// Map a `TestLog` prefixed line back into a [`TranscriptEntry`] —
    /// the inverse of the `"<role>: <body>"` shape the test impls push.
    fn test_log_entry(line: &str) -> TranscriptEntry {
        if let Some(rest) = line.strip_prefix("user: ") {
            TranscriptEntry::new(TranscriptRole::Operator, rest)
        } else if let Some(rest) = line.strip_prefix("assistant: ") {
            TranscriptEntry::new(TranscriptRole::Assistant, rest)
        } else if line.starts_with("tool[") {
            TranscriptEntry::new(TranscriptRole::Tool, line)
        } else {
            TranscriptEntry::new(TranscriptRole::System, line)
        }
    }

    /// Compaction-counter (file-scoped) — bumped by
    /// `CompactingLog::compact` and read by the run_loop integration
    /// test below. A static counter is the simplest way to thread the
    /// counter through `Provider::Log::from_briefing` without bloating
    /// the trait surface; the [`COMPACTION_TEST_LOCK`] mutex below
    /// serializes tests that touch this counter so they do not race.
    static COMPACT_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    /// Serialise tests that read/write [`COMPACT_CALL_COUNT`]. Cargo's
    /// default test runner spawns tests in parallel; without this lock
    /// the short-session and threshold-trigger tests would observe
    /// each other's counter increments.
    static COMPACTION_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// `MessageLog` instrumented with a static compaction counter.
    /// Tiny pre-seed — stays below the threshold; used for the
    /// short-session no-trigger test. The companion
    /// [`InflatedCompactingLog`] crosses the threshold and is used
    /// for the positive-trigger test.
    struct CompactingLog {
        messages: Vec<String>,
    }

    impl MessageLog for CompactingLog {
        type AssistantMsg = String;

        fn from_briefing(briefing: &str) -> Self {
            // Tiny pre-seed: ~16 KiB ≈ 4 k tokens, well below the
            // default 26 k-token threshold so this log NEVER trips
            // the spine's compaction trigger on its own.
            let mut messages = vec![format!("user: {briefing}")];
            for i in 0..40_usize {
                messages.push(format!("assistant: bulk turn {i}: {}", "x".repeat(400)));
            }
            Self { messages }
        }

        fn append_assistant(&mut self, msg: Self::AssistantMsg) {
            self.messages.push(format!("assistant: {msg}"));
        }

        fn append_tool_result(&mut self, call_id: &str, tool_name: &str, content: &str) {
            self.messages
                .push(format!("tool[{call_id}/{tool_name}]: {content}"));
        }

        fn append_user(&mut self, content: &str) {
            self.messages.push(format!("user: {content}"));
        }

        fn transcript(&self) -> Vec<TranscriptEntry> {
            self.messages.iter().map(|m| test_log_entry(m)).collect()
        }

        fn estimate_tokens(&self) -> u32 {
            u32::try_from(self.messages.iter().map(String::len).sum::<usize>() / 4)
                .unwrap_or(u32::MAX)
        }

        fn invariant_well_formed(&self) -> bool {
            true
        }

        fn compact(
            &mut self,
            target_tokens: u32,
            policy: CompactionPolicy,
        ) -> Result<CompactionReport, CompactionError> {
            COMPACT_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
            let tokens_before = self.estimate_tokens();
            if tokens_before <= target_tokens {
                return Err(CompactionError::NotApplicable);
            }
            let seed_len = self.messages.len().min(1);
            if self.messages.len() <= seed_len + policy.preserve_recent {
                return Err(CompactionError::NotApplicable);
            }
            let split = self.messages.len() - policy.preserve_recent;
            let messages_removed = split - seed_len;
            let mut new_messages = Vec::with_capacity(seed_len + 1 + policy.preserve_recent);
            new_messages.extend(self.messages[..seed_len].iter().cloned());
            new_messages.push("user: [compaction summary] earlier turns elided".to_owned());
            new_messages.extend(self.messages[split..].iter().cloned());
            self.messages = new_messages;
            Ok(CompactionReport {
                tokens_before,
                tokens_after: self.estimate_tokens(),
                messages_removed,
                summary_inserted: true,
            })
        }
    }

    /// Heavy variant of [`CompactingLog`] — pre-seeded with enough
    /// bulk that `estimate_tokens()` exceeds the default
    /// `CompactionPolicy::threshold_tokens(ContextBudget::DEFAULT)`
    /// from the first iteration of `run_loop`. Used to verify the
    /// positive compaction-trigger path.
    struct InflatedCompactingLog {
        messages: Vec<String>,
    }

    impl MessageLog for InflatedCompactingLog {
        type AssistantMsg = String;

        fn from_briefing(briefing: &str) -> Self {
            // Threshold for the 32_768-token default budget at 80 %
            // is ~26_214 tokens ≈ 104_000 chars. Seed ~130_000 chars
            // (~32_500 tokens), well above threshold but with the
            // SUMMARY message taking precedence so the post-compact
            // log lands at 60 % of budget.
            let mut messages = vec![format!("user: {briefing}")];
            for i in 0..130_usize {
                messages.push(format!("assistant: bulk turn {i}: {}", "x".repeat(1_000)));
            }
            Self { messages }
        }

        fn append_assistant(&mut self, msg: Self::AssistantMsg) {
            self.messages.push(format!("assistant: {msg}"));
        }

        fn append_tool_result(&mut self, call_id: &str, tool_name: &str, content: &str) {
            self.messages
                .push(format!("tool[{call_id}/{tool_name}]: {content}"));
        }

        fn append_user(&mut self, content: &str) {
            self.messages.push(format!("user: {content}"));
        }

        fn transcript(&self) -> Vec<TranscriptEntry> {
            self.messages.iter().map(|m| test_log_entry(m)).collect()
        }

        fn estimate_tokens(&self) -> u32 {
            u32::try_from(self.messages.iter().map(String::len).sum::<usize>() / 4)
                .unwrap_or(u32::MAX)
        }

        fn invariant_well_formed(&self) -> bool {
            true
        }

        fn compact(
            &mut self,
            target_tokens: u32,
            policy: CompactionPolicy,
        ) -> Result<CompactionReport, CompactionError> {
            COMPACT_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
            let tokens_before = self.estimate_tokens();
            if tokens_before <= target_tokens {
                return Err(CompactionError::NotApplicable);
            }
            let seed_len = self.messages.len().min(1);
            if self.messages.len() <= seed_len + policy.preserve_recent {
                return Err(CompactionError::NotApplicable);
            }
            let split = self.messages.len() - policy.preserve_recent;
            let messages_removed = split - seed_len;
            let mut new_messages = Vec::with_capacity(seed_len + 1 + policy.preserve_recent);
            new_messages.extend(self.messages[..seed_len].iter().cloned());
            new_messages.push("user: [compaction summary] earlier turns elided".to_owned());
            new_messages.extend(self.messages[split..].iter().cloned());
            self.messages = new_messages;
            Ok(CompactionReport {
                tokens_before,
                tokens_after: self.estimate_tokens(),
                messages_removed,
                summary_inserted: true,
            })
        }
    }

    /// Scripted provider over [`CompactingLog`] — emits Stop on the
    /// first call, so `run_loop` performs exactly one
    /// compaction-trigger check.
    struct CompactingProvider {
        text: String,
    }

    #[async_trait]
    impl Provider for CompactingProvider {
        type Log = CompactingLog;
        type Error = std::io::Error;

        async fn one_turn(&self, _log: &Self::Log) -> Result<Turn<Self::Log>, Self::Error> {
            Ok(Turn::Stop(self.text.clone()))
        }

        fn tool_schema(&self) -> Vec<ToolDeclaration> {
            default_registry().declarations()
        }
    }

    /// Same as [`CompactingProvider`] but binds the heavy log.
    struct InflatedCompactingProvider {
        text: String,
    }

    #[async_trait]
    impl Provider for InflatedCompactingProvider {
        type Log = InflatedCompactingLog;
        type Error = std::io::Error;

        async fn one_turn(&self, _log: &Self::Log) -> Result<Turn<Self::Log>, Self::Error> {
            Ok(Turn::Stop(self.text.clone()))
        }

        fn tool_schema(&self) -> Vec<ToolDeclaration> {
            default_registry().declarations()
        }
    }

    /// Scripted provider — pops a turn off a stack on each call.
    struct ScriptedProvider {
        script: Mutex<Vec<Turn<TestLog>>>,
    }

    impl ScriptedProvider {
        fn new(script: Vec<Turn<TestLog>>) -> Self {
            // The spine pops from the *front*; store reversed so
            // `pop()` returns the next turn in script order.
            let mut reversed = script;
            reversed.reverse();
            Self {
                script: Mutex::new(reversed),
            }
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        type Log = TestLog;
        type Error = std::io::Error;

        async fn one_turn(&self, _log: &Self::Log) -> Result<Turn<Self::Log>, Self::Error> {
            let mut script = self.script.lock().unwrap();
            script
                .pop()
                .ok_or_else(|| std::io::Error::other("script exhausted"))
        }

        fn tool_schema(&self) -> Vec<ToolDeclaration> {
            default_registry().declarations()
        }
    }

    #[tokio::test]
    async fn run_loop_returns_stop_text() {
        let dir = tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![Turn::Stop("haiku here".to_owned())]);
        let result = run_loop(&provider, "write a haiku", dir.path(), None)
            .await
            .expect("loop must terminate cleanly");
        assert_eq!(result, "haiku here");
    }

    #[tokio::test]
    async fn run_loop_dispatches_tool_then_stops() {
        let dir = tempdir().unwrap();
        // Seed a file so `read_file` (one of the v0 registry tools)
        // has something to return. delib-20260518-5178 C2 retired
        // `write_file`; using `read_file` keeps this smoke test
        // tied to the actual v0 surface.
        std::fs::write(dir.path().join("haiku.md"), "spine drives the loop").unwrap();
        let provider = ScriptedProvider::new(vec![
            Turn::ToolCalls {
                assistant: "calling read_file".to_owned(),
                calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({
                        "path": "haiku.md",
                    })
                    .to_string(),
                }],
            },
            Turn::Stop("done".to_owned()),
        ]);
        let result = run_loop(&provider, "read the haiku at haiku.md", dir.path(), None)
            .await
            .expect("loop must terminate cleanly");
        assert_eq!(result, "done");
    }

    /// A tool-execution failure must NOT abort the loop — the typed
    /// error is fed back to the model as the tool result so it can
    /// recover on the next turn. Here the model
    /// reads a non-existent file (ENOENT), sees the error, then stops
    /// cleanly. Before the fix, `run_loop` returned
    /// `Err(HarnessError::Tool(..))` and the molecule died on the first
    /// fumbled tool call — brittle against local Ollama models.
    #[tokio::test]
    async fn run_loop_recovers_from_tool_error_instead_of_aborting() {
        let dir = tempdir().unwrap();
        // Script order (ScriptedProvider::new stores reversed so the
        // first element runs first): a failing tool call, then a Stop.
        let provider = ScriptedProvider::new(vec![
            Turn::ToolCalls {
                assistant: "reading a file that does not exist".to_owned(),
                calls: vec![ToolCall {
                    id: "call-err".to_owned(),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({
                        "path": "does-not-exist.md",
                    })
                    .to_string(),
                }],
            },
            Turn::Stop("recovered".to_owned()),
        ]);
        let result = run_loop(&provider, "read a missing file", dir.path(), None)
            .await
            .expect("a tool IO failure must NOT abort the loop");
        assert_eq!(
            result, "recovered",
            "the loop must continue past a failed tool call and reach the model's Stop",
        );
    }

    #[tokio::test]
    async fn run_loop_rejects_oversized_briefing() {
        let dir = tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![Turn::Stop("never reached".to_owned())]);
        let huge = "x".repeat((ContextBudget::DEFAULT.max_input_tokens as usize) * 4 + 64);
        let err = run_loop(&provider, &huge, dir.path(), None)
            .await
            .expect_err("must refuse");
        assert!(matches!(err, HarnessError::ContextOverflow { .. }));
    }

    /// Context-budget in-loop enforcement.
    /// A briefing that fits pre-flight but grows past the context cap
    /// once tool results land must return `ContextOverflow`, not
    /// `Provider(...)`.
    #[tokio::test]
    async fn run_loop_enforces_context_overflow_inside_loop() {
        let dir = tempdir().unwrap();
        // Seed a *distinct* 50 KiB file per turn — each `read_file` result
        // is folded into the log, so a handful of turns saturate the
        // context cap. Distinct files keep the work varied so the C4
        // cycle guard (which would fire on reading the *same* file 3×)
        // does not pre-empt the I3 overflow this test exercises.
        let payload = "x".repeat(50 * 1024);

        // Build a script of 10 `read_file` calls so the log
        // monotonically grows. The provider never emits Stop.
        let mut script = Vec::new();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("big-{i}.txt")), &payload).unwrap();
            script.push(Turn::ToolCalls {
                assistant: format!("turn {i}"),
                calls: vec![ToolCall {
                    id: format!("call-{i}"),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({
                        "path": format!("big-{i}.txt"),
                    })
                    .to_string(),
                }],
            });
        }
        let provider = ScriptedProvider::new(script);
        let err = run_loop(&provider, "loop and read", dir.path(), None)
            .await
            .expect_err("must trip I3 mid-loop");
        match err {
            HarnessError::ContextOverflow { limit, .. } => {
                assert_eq!(limit, ContextBudget::DEFAULT.max_input_tokens);
            }
            other => panic!("expected ContextOverflow, got {other:?}"),
        }
    }

    /// Tool-budget enforcement. A scripted
    /// turn with 65 tool_calls must trip `ToolBudgetExhausted` on the
    /// 65th call, never silently dispatching 65 tool bodies.
    #[tokio::test]
    async fn run_loop_enforces_tool_budget_in_single_turn() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();
        let n = (crate::budget::ToolBudget::DEFAULT.max_tool_calls as usize) + 1;
        let calls = (0..n)
            .map(|i| ToolCall {
                id: format!("call-{i}"),
                name: "read_file".to_owned(),
                arguments_json: serde_json::json!({ "path": "note.txt" }).to_string(),
            })
            .collect::<Vec<_>>();
        let provider = ScriptedProvider::new(vec![Turn::ToolCalls {
            assistant: "fan out".to_owned(),
            calls,
        }]);
        let err = run_loop(&provider, "fan out tool calls", dir.path(), None)
            .await
            .expect_err("must trip I2");
        match err {
            HarnessError::ToolBudgetExhausted { limit } => {
                assert_eq!(limit, crate::budget::ToolBudget::DEFAULT.max_tool_calls);
            }
            other => panic!("expected ToolBudgetExhausted, got {other:?}"),
        }
    }

    /// C4 mechanism 5 — a weak oracle that emits the *same* `read_file` call
    /// on three consecutive turns is stuck in a loop; the spine catches the
    /// tool-call cycle and halts before the turn/tool budgets drain. The
    /// halt fires strictly *earlier* than `TurnBudgetExhausted` would, so the
    /// termination proof is preserved.
    #[tokio::test]
    async fn run_loop_detects_tool_call_cycle() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();
        let same_call = || Turn::ToolCalls {
            assistant: "reading again".to_owned(),
            calls: vec![ToolCall {
                id: "unique-id-varies".to_owned(),
                name: "read_file".to_owned(),
                arguments_json: serde_json::json!({ "path": "note.txt" }).to_string(),
            }],
        };
        // Three identical single-call turns → period-1 cycle at the default
        // threshold (DEFAULT_CYCLE_REPEATS = 3). A trailing Stop proves the
        // cycle fires *before* the loop would otherwise reach it.
        let provider = ScriptedProvider::new(vec![
            same_call(),
            same_call(),
            same_call(),
            Turn::Stop("unreachable".to_owned()),
        ]);
        let err = run_loop(&provider, "get stuck", dir.path(), None)
            .await
            .expect_err("three identical calls must trip the cycle detector");
        match err {
            HarnessError::ToolCallCycle { period, repeats } => {
                assert_eq!(period, 1);
                assert_eq!(repeats, cosmon_core::oracle_boundary::DEFAULT_CYCLE_REPEATS);
            }
            other => panic!("expected ToolCallCycle, got {other:?}"),
        }
    }

    /// The dual of the cycle test: two identical calls are NOT a cycle
    /// (a confirm-loop is legitimate), so the loop proceeds to `Stop`.
    #[tokio::test]
    async fn run_loop_allows_two_identical_calls() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();
        let same_call = || Turn::ToolCalls {
            assistant: "reading".to_owned(),
            calls: vec![ToolCall {
                id: "id".to_owned(),
                name: "read_file".to_owned(),
                arguments_json: serde_json::json!({ "path": "note.txt" }).to_string(),
            }],
        };
        let provider = ScriptedProvider::new(vec![
            same_call(),
            same_call(),
            Turn::Stop("done".to_owned()),
        ]);
        let out = run_loop(&provider, "confirm-loop", dir.path(), None)
            .await
            .expect("two identical calls are a confirm-loop, not a cycle");
        assert_eq!(out, "done");
    }

    /// Task-20260521-d436 — the spine's compaction trigger fires when
    /// the log's estimated tokens exceed
    /// `CompactionPolicy::DEFAULT.threshold_tokens(ContextBudget::DEFAULT)`.
    /// The [`InflatedCompactingLog`] pre-seeds ~32 k tokens (above the
    /// 26 k threshold), so the spine's pre-turn compaction trigger
    /// fires once and reduces the log below the I3 ceiling.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn run_loop_triggers_compaction_when_log_exceeds_threshold() {
        let _guard = COMPACTION_TEST_LOCK.lock().unwrap();
        COMPACT_CALL_COUNT.store(0, Ordering::SeqCst);
        let dir = tempdir().unwrap();
        let provider = InflatedCompactingProvider {
            text: "done".into(),
        };
        let _ = run_loop(&provider, "brief", dir.path(), None)
            .await
            .expect("loop runs to Stop after compaction");
        assert!(
            COMPACT_CALL_COUNT.load(Ordering::SeqCst) >= 1,
            "compaction must fire when pre-seed exceeds threshold; got {}",
            COMPACT_CALL_COUNT.load(Ordering::SeqCst)
        );
    }

    /// Direct exercise of the `CompactingLog::compact` policy logic —
    /// independent of `run_loop`'s threshold computation. Verifies
    /// the trait-method wiring: compaction reduces the log, sets
    /// `summary_inserted`, and reports non-zero `messages_removed`.
    #[test]
    fn compacting_log_reduces_size_when_over_target() {
        let mut log = CompactingLog::from_briefing("brief");
        let tokens_before = log.estimate_tokens();
        let report = log
            .compact(100, CompactionPolicy::DEFAULT)
            .expect("compaction succeeds");
        assert!(report.summary_inserted);
        assert!(report.messages_removed > 0);
        assert!(report.tokens_after < tokens_before);
        assert!(log.invariant_well_formed());
    }

    /// Short sessions must NOT trigger compaction. With a tiny
    /// briefing the spine should run to completion without ever
    /// calling `MessageLog::compact`.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn run_loop_does_not_compact_short_session() {
        let _guard = COMPACTION_TEST_LOCK.lock().unwrap();
        COMPACT_CALL_COUNT.store(0, Ordering::SeqCst);
        let dir = tempdir().unwrap();
        // CompactingLog::from_briefing seeds 40 × 400 chars = 16 k
        // chars = ~4 k tokens, well below the 26 k threshold derived
        // from ContextBudget::DEFAULT × 0.8.
        let provider = CompactingProvider {
            text: "done".into(),
        };
        let _ = run_loop(&provider, "short brief", dir.path(), None)
            .await
            .expect("loop runs to Stop");
        assert_eq!(
            COMPACT_CALL_COUNT.load(Ordering::SeqCst),
            0,
            "short session must not trigger compaction"
        );
    }

    #[tokio::test]
    async fn run_loop_exhausts_turn_budget() {
        let dir = tempdir().unwrap();
        // Seed a *distinct* file per turn so each `read_file` is genuine,
        // varied work — this isolates the 30-turn cap from the C4 cycle
        // guard (reading the *same* file 30× would trip the tool-call
        // cycle detector first, which is the correct behaviour for a stuck
        // loop but not what this test exercises). The script never emits a
        // Stop, so the turn budget kicks in.
        let mut script = Vec::new();
        for i in 0..(TurnBudget::DEFAULT.max_turns as usize) {
            std::fs::write(dir.path().join(format!("note-{i}.txt")), "x").unwrap();
            script.push(Turn::ToolCalls {
                assistant: format!("turn {i}"),
                calls: vec![ToolCall {
                    id: format!("call-{i}"),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({
                        "path": format!("note-{i}.txt"),
                    })
                    .to_string(),
                }],
            });
        }
        let provider = ScriptedProvider::new(script);
        let err = run_loop(&provider, "loop forever", dir.path(), None)
            .await
            .expect_err("must hit turn cap");
        assert!(matches!(err, HarnessError::TurnBudgetExhausted { .. }));
    }

    /// `ScriptedProviderFn` demonstration — the closure inspects the
    /// log on every call and asserts that, after a [`Turn::ToolCalls`]
    /// turn, the spine has already appended (a) the assistant
    /// envelope, (b) the matching tool_result, in that order, before
    /// dispatching the next [`Provider::one_turn`].
    ///
    /// This catches re-priming bugs that the `Vec<Turn>`-popping
    /// scripted provider cannot — that double ignores the log
    /// argument, so a spine that forgot to call `append_tool_result`
    /// (or appended it before `append_assistant`) would still pass
    /// every existing test.
    #[tokio::test]
    async fn scripted_provider_fn_observes_tool_result_in_log_between_turns() {
        let dir = tempdir().unwrap();
        // Seed a file so `read_file` (in the default registry) has
        // something to return — keeps the test tied to the v0
        // tool surface, same discipline as
        // `run_loop_dispatches_tool_then_stops` above.
        std::fs::write(dir.path().join("data.txt"), "hello").unwrap();

        let observed_turn2 = std::sync::Arc::new(Mutex::new(None::<Vec<String>>));
        let observed_in = std::sync::Arc::clone(&observed_turn2);
        let turn_index = std::sync::Arc::new(AtomicUsize::new(0));
        let turn_index_in = std::sync::Arc::clone(&turn_index);

        let provider = ScriptedProviderFn::<TestLog, std::io::Error>::new(move |log: &TestLog| {
            let n = turn_index_in.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First call: log holds just the briefing.
                assert_eq!(
                    log.messages.len(),
                    1,
                    "turn 0 must see only the briefing user message; got {:?}",
                    log.messages
                );
                Ok(Turn::ToolCalls {
                    assistant: "calling read_file".to_owned(),
                    calls: vec![ToolCall {
                        id: "call-1".to_owned(),
                        name: "read_file".to_owned(),
                        arguments_json: serde_json::json!({
                            "path": "data.txt",
                        })
                        .to_string(),
                    }],
                })
            } else {
                // Snapshot the log so the test body can assert
                // ordering after run_loop returns.
                *observed_in.lock().unwrap() = Some(log.messages.clone());
                Ok(Turn::Stop("done".to_owned()))
            }
        });

        let result = run_loop(&provider, "read it", dir.path(), None)
            .await
            .expect("loop must terminate");
        assert_eq!(result, "done");

        let snap = observed_turn2.lock().unwrap();
        let messages = snap
            .as_ref()
            .expect("provider must have been called a second time");

        // Expected ordering on turn 2:
        //   [0] "user: <briefing>"
        //   [1] "assistant: calling read_file"
        //   [2] "tool[call-1/read_file]: hello"
        assert_eq!(
            messages.len(),
            3,
            "log must hold briefing + assistant + tool_result by turn 2; got {messages:?}"
        );
        assert!(
            messages[0].starts_with("user:"),
            "messages[0] must remain the briefing; got {:?}",
            messages[0]
        );
        assert!(
            messages[1].starts_with("assistant:"),
            "messages[1] must be the assistant turn that requested the call; got {:?}",
            messages[1]
        );
        assert!(
            messages[2].starts_with("tool[call-1/read_file]"),
            "messages[2] must be the tool_result appended after the assistant turn; got {:?}",
            messages[2]
        );
    }

    /// `ScriptedProviderFn` smoke test — the closure can drive the
    /// loop through a single [`Turn::Stop`] without ever calling a
    /// tool. The default tool schema is the v0 registry.
    #[tokio::test]
    async fn scripted_provider_fn_drives_stop_in_one_turn() {
        let dir = tempdir().unwrap();
        let provider = ScriptedProviderFn::<TestLog, std::io::Error>::new(|_log: &TestLog| {
            Ok(Turn::Stop("first-and-last".to_owned()))
        });
        let result = run_loop(&provider, "just stop", dir.path(), None)
            .await
            .expect("loop must terminate cleanly");
        assert_eq!(result, "first-and-last");
        assert_eq!(
            provider.tool_schema().len(),
            default_registry().declarations().len(),
            "default tool schema must come from the v0 registry"
        );
    }

    // ----------------------------------------------------------------
    // Interactive `step()` path (ADR-115 / cs_pilot_interactive_fsm).
    //
    // These exercise `InteractiveSession`, the caller-driven counterpart
    // to `run_loop`. The load-bearing property under test is the TLA+
    // `InteractiveStopYields` invariant: a model `Turn::Stop` must YIELD
    // to the operator (loop back to `awaiting`), never terminate the
    // session. `StepOutcome` encodes this in the type — there is no
    // terminal variant — so these tests assert the *runtime* behaviour
    // that backs the type-level guarantee.
    // ----------------------------------------------------------------

    /// The brief's named scenario: operator-turn → tool-call → yield-on-
    /// stop. The session must (1) accept an operator turn, (2) dispatch
    /// the model's tool call and return `Continued`, (3) on the model's
    /// `Turn::Stop` return `Yielded` and re-enter `awaiting` — NOT
    /// terminate — and (4) accept a *second* operator turn, proving the
    /// loop-back the worker path cannot do.
    #[tokio::test]
    async fn interactive_session_operator_turn_tool_call_yield_on_stop() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("data.txt"), "fleet is quiescent").unwrap();

        // Script: tool call, stop (turn 1), then stop again (turn 2).
        // ScriptedProvider stores reversed internally so this is the
        // dispatch order.
        let provider = ScriptedProvider::new(vec![
            Turn::ToolCalls {
                assistant: "reading data.txt".to_owned(),
                calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({ "path": "data.txt" }).to_string(),
                }],
            },
            Turn::Stop("the fleet looks calm".to_owned()),
            Turn::Stop("still calm".to_owned()),
        ]);

        let mut session = InteractiveSession::new(provider, "you are cs pilot", dir.path())
            .expect("session opens within the context budget");
        assert!(
            session.is_awaiting_operator(),
            "a fresh session starts in the awaiting state (FSM Init)"
        );

        // (1) Operator submits a turn → leaves awaiting.
        session.submit("how is the fleet?");
        assert!(!session.is_awaiting_operator());
        assert_eq!(session.turn_count(), 0, "submit resets the per-turn budget");

        // (2) First step dispatches the tool call and continues.
        let outcome = session.step().await.expect("step must not error");
        assert!(
            matches!(outcome, StepOutcome::Continued),
            "a tool-call turn must continue, not yield; got {outcome:?}"
        );
        assert_eq!(session.turn_count(), 1, "one round-trip spent");
        assert_eq!(session.used_tools(), 1, "one tool dispatched");
        assert!(
            !session.is_awaiting_operator(),
            "still mid-turn after a tool dispatch"
        );

        // (3) Second step hits Turn::Stop → YIELDS, does NOT terminate.
        let outcome = session.step().await.expect("step must not error");
        match outcome {
            StepOutcome::Yielded(text) => assert_eq!(text, "the fleet looks calm"),
            other => panic!("Turn::Stop must yield to the operator; got {other:?}"),
        }
        assert!(
            session.is_awaiting_operator(),
            "yielding re-enters the awaiting state (InteractiveStopYields)"
        );

        // (4) The session is alive: a second operator turn is accepted
        // and drives the model again. This is the loop the one-shot
        // worker path cannot express.
        session.submit("anything else?");
        assert!(!session.is_awaiting_operator());
        assert_eq!(session.turn_count(), 0, "second turn resets the budget");
        let outcome = session.step().await.expect("step must not error");
        assert!(
            matches!(outcome, StepOutcome::Yielded(ref t) if t == "still calm"),
            "second operator turn yields again; got {outcome:?}"
        );
        assert!(session.is_awaiting_operator());
    }

    /// `transcript()` is the read accessor the REPL paints scrollback
    /// from — it must reflect the operator turn, the assistant turn, and
    /// the tool result, in chronological order.
    #[tokio::test]
    async fn interactive_session_transcript_renders_history() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("data.txt"), "hello").unwrap();

        let provider = ScriptedProvider::new(vec![
            Turn::ToolCalls {
                assistant: "calling read_file".to_owned(),
                calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({ "path": "data.txt" }).to_string(),
                }],
            },
            Turn::Stop("done".to_owned()),
        ]);
        let mut session = InteractiveSession::new(provider, "briefing", dir.path()).unwrap();
        session.submit("read the file");
        assert!(matches!(
            session.step().await.unwrap(),
            StepOutcome::Continued
        ));
        assert!(matches!(
            session.step().await.unwrap(),
            StepOutcome::Yielded(_)
        ));

        let transcript = session.transcript();
        // Expected order: briefing (Operator), the submitted operator
        // line (Operator), the assistant turn, the tool result.
        let roles: Vec<TranscriptRole> = transcript.iter().map(|e| e.role).collect();
        assert_eq!(
            roles,
            vec![
                TranscriptRole::Operator, // briefing seed
                TranscriptRole::Operator, // "read the file"
                TranscriptRole::Assistant,
                TranscriptRole::Tool,
            ],
            "transcript must read chronologically; got {transcript:?}"
        );
        assert_eq!(transcript[1].content, "read the file");
        assert_eq!(transcript[2].content, "calling read_file");
    }

    /// `with_registry` injects a caller-supplied tool registry in place of
    /// the [`default_registry`] — the cs-pilot REPL's hook to advertise the
    /// cosmon-ops tools instead of the filesystem trio. Proof it is the
    /// *injected* registry that governs dispatch: script a `read_file`
    /// call (present in the default registry, absent from an empty one) and
    /// assert it comes back as a `NotWhitelisted` error fed to the model.
    #[tokio::test]
    async fn interactive_session_with_registry_uses_the_injected_registry() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("data.txt"), "present").unwrap();

        let provider = ScriptedProvider::new(vec![
            Turn::ToolCalls {
                assistant: "trying read_file".to_owned(),
                calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({ "path": "data.txt" }).to_string(),
                }],
            },
            Turn::Stop("done".to_owned()),
        ]);

        // Empty registry — read_file is NOT registered here even though it
        // would be in `default_registry()`.
        let mut session = InteractiveSession::with_registry(
            provider,
            "briefing",
            dir.path(),
            ToolRegistry::new(),
        )
        .expect("session opens");
        session.submit("read the file");
        assert!(matches!(
            session.step().await.unwrap(),
            StepOutcome::Continued
        ));
        assert!(matches!(
            session.step().await.unwrap(),
            StepOutcome::Yielded(_)
        ));

        // The tool result must be the NotWhitelisted error — proving the
        // empty injected registry, not the default, was consulted.
        let transcript = session.transcript();
        let tool_entry = transcript
            .iter()
            .find(|e| e.role == TranscriptRole::Tool)
            .expect("a tool result was folded back");
        assert!(
            tool_entry.content.contains("not whitelisted"),
            "injected empty registry must refuse read_file; got {:?}",
            tool_entry.content
        );
    }

    /// A spent per-turn budget must YIELD (`BudgetExhausted`), not error.
    /// Contrast `run_loop_exhausts_turn_budget`, where the same condition
    /// returns `Err(TurnBudgetExhausted)` — the worker has no operator to
    /// yield to (TLA+ `BudgetExhausted`: interactive → yield, worker →
    /// stopped).
    #[tokio::test]
    async fn interactive_session_budget_exhausted_yields_not_errors() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();

        // A provider that NEVER stops — always asks to call a tool.
        let provider = ScriptedProviderFn::<TestLog, std::io::Error>::new(|_log: &TestLog| {
            Ok(Turn::ToolCalls {
                assistant: "looping".to_owned(),
                calls: vec![ToolCall {
                    id: "call-x".to_owned(),
                    name: "read_file".to_owned(),
                    arguments_json: serde_json::json!({ "path": "note.txt" }).to_string(),
                }],
            })
        });
        let mut session = InteractiveSession::new(provider, "brief", dir.path()).unwrap();
        session.submit("loop forever");

        let max = TurnBudget::DEFAULT.max_turns;
        // The first `max` steps each dispatch a tool and continue.
        for n in 0..max {
            let outcome = session
                .step()
                .await
                .expect("step must not error mid-budget");
            assert!(
                matches!(outcome, StepOutcome::Continued),
                "round-trip {n} must continue; got {outcome:?}"
            );
        }
        // The next step finds the per-turn budget spent and yields.
        let outcome = session
            .step()
            .await
            .expect("budget exhaustion must NOT error");
        match outcome {
            StepOutcome::BudgetExhausted { limit } => assert_eq!(limit, max),
            other => panic!("a spent turn budget must yield, not error; got {other:?}"),
        }
        assert!(
            session.is_awaiting_operator(),
            "budget exhaustion hands control back to the operator"
        );
        // And the session survives: submitting again resets the budget.
        session.submit("try again");
        assert_eq!(session.turn_count(), 0);
        assert!(!session.is_awaiting_operator());
    }

    /// A runaway tool fan-out IS fatal in the interactive regime too —
    /// `ToolBudgetExhausted` is an error, not a yield (unlike a spent
    /// *turn* budget). Mirrors `run_loop_enforces_tool_budget_in_single_turn`.
    #[tokio::test]
    async fn interactive_session_tool_budget_is_fatal() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();
        let n = (ToolBudget::DEFAULT.max_tool_calls as usize) + 1;
        let calls = (0..n)
            .map(|i| ToolCall {
                id: format!("call-{i}"),
                name: "read_file".to_owned(),
                arguments_json: serde_json::json!({ "path": "note.txt" }).to_string(),
            })
            .collect::<Vec<_>>();
        let provider = ScriptedProvider::new(vec![Turn::ToolCalls {
            assistant: "fan out".to_owned(),
            calls,
        }]);
        let mut session = InteractiveSession::new(provider, "brief", dir.path()).unwrap();
        session.submit("fan out tool calls");
        let err = session
            .step()
            .await
            .expect_err("a 65-call fan-out must trip I2");
        assert!(matches!(err, HarnessError::ToolBudgetExhausted { .. }));
    }

    /// The interactive constructor enforces the same I3 pre-flight gate
    /// as `run_loop` — an oversized briefing is refused at `new`, before
    /// any operator turn.
    #[tokio::test]
    async fn interactive_session_rejects_oversized_briefing() {
        let dir = tempdir().unwrap();
        let provider = ScriptedProviderFn::<TestLog, std::io::Error>::new(|_l: &TestLog| {
            Ok(Turn::Stop("never reached".to_owned()))
        });
        let huge = "x".repeat((ContextBudget::DEFAULT.max_input_tokens as usize) * 4 + 64);
        let err = InteractiveSession::new(provider, &huge, dir.path())
            .err()
            .expect("oversized briefing must be refused at construction");
        assert!(matches!(err, HarnessError::ContextOverflow { .. }));
    }
}
