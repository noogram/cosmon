-------------------------- MODULE cs_pilot_interactive_fsm --------------------------
(* cs_pilot_interactive_fsm — TLA+ model of the interactive harness FSM that
   the `cs pilot` `step()` refactor introduces (ADR-115).

   Naming: snake_case, not the kebab `cs-pilot-interactive-fsm` of the task
   brief. TLA+ module names forbid hyphens and TLC enforces filename ==
   module name; the kebab form cannot be model-checked. Forced kebab->snake
   rename — the same KebabRenameBait the repo tracks in CosmonDocHarness.tla.

   Parent ADR : docs/adr/115-cs-pilot-cognitive-pilot.md
   Spec source: docs/delib-prep/2026-05-31-cs-pilot-external-cognitive-pilot.md (§4, §7 #1)
   Substrate  : cosmon-agent-harness::spine — today's one-shot `run_loop<P>`
                terminates on `Turn::Stop`; the REPL caller must instead YIELD.

   ------------------------------------------------------------------------
   The six FSM states (task brief vocabulary -> short token used here):

     awaiting-operator-input  ->  "awaiting"
     sending-to-model         ->  "sending"
     decoding                 ->  "decoding"
     dispatching-tool         ->  "dispatching"
     yield-to-operator        ->  "yield"
     stopped                  ->  "stopped"   (terminal absorbing state)

   A single `mode` variable, chosen non-deterministically at Init, lets ONE
   model cover both callers of `step()`:

     mode = "interactive"  ->  the new `cs pilot` REPL: the REPL owns the
                               loop; on Turn::Stop the harness yields to the
                               operator and loops back to "awaiting".
     mode = "worker"       ->  the preserved one-shot worker path: a single
                               immutable briefing in, Turn::Stop terminates
                               (return Ok(synthesis)), no operator yield.

   ------------------------------------------------------------------------
   The LOAD-BEARING invariant (ADR-115):

     InteractiveStopYields ==
         (mode = "interactive" /\ pc = "stopped") => operator_quit

   "In an interactive session the harness is in `stopped` ONLY because the
    operator explicitly quit. A model Turn::Stop never silently terminates
    the interactive session — it routes to `yield`, which loops back to
    `awaiting`."  `operator_quit` is set TRUE by exactly one action:
    OperatorQuit (the `/quit` pilot directive).

   The spec carries a dormant, constant-guarded `SilentTerminate` action that
   models the bug the invariant forbids (interactive Turn::Stop -> stopped,
   operator never asked). With AllowSilentTerminate = TRUE, TLC exhibits the
   counterexample — proof the invariant is load-bearing, not vacuous. The
   shipped .cfg keeps it FALSE and the model checks clean. *)
EXTENDS Naturals

CONSTANTS
    MaxTurns,            \* cap on model<->tool round-trips inside one operator turn
    MaxOperatorTurns,    \* cap on operator turns served (bounds the state space)
    AllowSilentTerminate \* BOOLEAN — enable the bug action to exhibit the counterexample

ASSUME /\ MaxTurns         \in Nat /\ MaxTurns         > 0
       /\ MaxOperatorTurns \in Nat /\ MaxOperatorTurns > 0
       /\ AllowSilentTerminate \in BOOLEAN

----------------------------------------------------------------------------
States == { "awaiting", "sending", "decoding", "dispatching", "yield", "stopped" }

\* The "busy" region: the harness is mid-computation, not quiescent.
\* No-livelock asserts the busy region is always eventually exited.
BusyStates == { "sending", "decoding", "dispatching", "yield" }
Quiescent  == { "awaiting", "stopped" }

----------------------------------------------------------------------------
VARIABLES
    mode,           \* "interactive" | "worker" — fixed at Init, never mutated
    pc,             \* current FSM state \in States
    operator_quit,  \* BOOLEAN — TRUE iff the operator issued /quit
    turn_count,     \* 0..MaxTurns — model<->tool round-trips in the current turn
    op_turns        \* 0..MaxOperatorTurns — operator turns served (forensic clock)

vars == << mode, pc, operator_quit, turn_count, op_turns >>

----------------------------------------------------------------------------
\* Init — the interactive REPL starts awaiting the operator; the one-shot
\* worker starts already sending its immutable briefing to the model.

Init ==
    /\ mode \in { "interactive", "worker" }
    /\ pc = IF mode = "interactive" THEN "awaiting" ELSE "sending"
    /\ operator_quit = FALSE
    /\ turn_count = 0
    /\ op_turns = 0

----------------------------------------------------------------------------
\* Interactive-only operator actions.

\* OperatorSubmit — the operator types a turn at the ❯ prompt. Resets the
\* per-turn budget and advances the forensic operator-turn clock.
OperatorSubmit ==
    /\ mode = "interactive"
    /\ pc = "awaiting"
    /\ op_turns < MaxOperatorTurns
    /\ pc' = "sending"
    /\ turn_count' = 0
    /\ op_turns' = op_turns + 1
    /\ UNCHANGED << mode, operator_quit >>

\* OperatorQuit — the `/quit` pilot directive. This is the ONLY way an
\* interactive session legitimately reaches `stopped`, and the ONLY action
\* that sets operator_quit. (A model Turn::Stop must never do this.)
OperatorQuit ==
    /\ mode = "interactive"
    /\ pc = "awaiting"
    /\ pc' = "stopped"
    /\ operator_quit' = TRUE
    /\ UNCHANGED << mode, turn_count, op_turns >>

----------------------------------------------------------------------------
\* Internal harness actions (shared by both modes — these are `step()`).

\* SendToModel — emit the current MessageLog to the provider; counts one
\* model round-trip against the per-turn budget.
SendToModel ==
    /\ pc = "sending"
    /\ turn_count < MaxTurns
    /\ pc' = "decoding"
    /\ turn_count' = turn_count + 1
    /\ UNCHANGED << mode, operator_quit, op_turns >>

\* BudgetExhausted — the per-turn budget is spent and the model still wants
\* to continue (HarnessError::TurnBudgetExhausted). Crucially, in interactive
\* mode this YIELDS to the operator (never silently stops); in worker mode it
\* terminates with the harness error.
BudgetExhausted ==
    /\ pc = "sending"
    /\ turn_count >= MaxTurns
    /\ pc' = IF mode = "interactive" THEN "yield" ELSE "stopped"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>

\* DecodeToolCalls — the decoded Turn is Turn::ToolCalls.
DecodeToolCalls ==
    /\ pc = "decoding"
    /\ pc' = "dispatching"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>

\* DecodeStop — the decoded Turn is Turn::Stop. THE load-bearing branch:
\* interactive -> yield to operator ; worker -> terminate.
DecodeStop ==
    /\ pc = "decoding"
    /\ pc' = IF mode = "interactive" THEN "yield" ELSE "stopped"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>

\* DispatchTool — execute the tool call, append the result, feed it back to
\* the model. A failed tool is recovered (loop continues), never aborts —
\* same shape as today's run_loop.
DispatchTool ==
    /\ pc = "dispatching"
    /\ pc' = "sending"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>

\* YieldToOperator — render the synthesis and HAND CONTROL BACK. This is the
\* transition that makes the interactive session a loop instead of a
\* one-shot: it returns to "awaiting", never to "stopped".
YieldToOperator ==
    /\ mode = "interactive"
    /\ pc = "yield"
    /\ pc' = "awaiting"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>

----------------------------------------------------------------------------
\* The forbidden BUG — dormant unless AllowSilentTerminate is set. Models a
\* refactor that wires the interactive caller's Turn::Stop straight to the
\* one-shot terminator, silently killing the session without operator intent.
\* This is precisely what InteractiveStopYields forbids.
SilentTerminate ==
    /\ AllowSilentTerminate
    /\ mode = "interactive"
    /\ pc = "decoding"
    /\ pc' = "stopped"
    /\ operator_quit' = FALSE        \* silent: the operator never asked
    /\ UNCHANGED << mode, turn_count, op_turns >>

----------------------------------------------------------------------------
\* The internal `step()` actions — weak fairness lives here so the busy
\* region is always eventually exited (no-livelock). Operator actions are
\* deliberately NOT fair: the operator may idle at the ❯ prompt forever.
Internal ==
    \/ SendToModel
    \/ BudgetExhausted
    \/ DecodeToolCalls
    \/ DecodeStop
    \/ DispatchTool
    \/ YieldToOperator

Next ==
    \/ OperatorSubmit
    \/ OperatorQuit
    \/ Internal
    \/ SilentTerminate

Spec == Init /\ [][Next]_vars /\ WF_vars(Internal)

----------------------------------------------------------------------------
\* Invariants.

TypeOK ==
    /\ mode \in { "interactive", "worker" }
    /\ pc \in States
    /\ operator_quit \in BOOLEAN
    /\ turn_count \in 0..MaxTurns
    /\ op_turns \in 0..MaxOperatorTurns

\* THE load-bearing invariant. In an interactive session, `stopped` is
\* reachable ONLY through an explicit operator /quit.
InteractiveStopYields ==
    (mode = "interactive" /\ pc = "stopped") => operator_quit

\* The refactor must not disturb the one-shot worker path: worker mode never
\* visits the interactive-only states. (Turn::Stop terminates, as today.)
WorkerPathUnchanged ==
    (mode = "worker") => (pc \notin { "awaiting", "yield" })

\* Turn-boundedness: the model<->tool ping-pong inside a single operator turn
\* is bounded (subsumed by TypeOK, stated separately as the named property).
TurnBounded == turn_count \in 0..MaxTurns

\* A worker that has terminated has done so without ever setting operator_quit
\* (there is no operator in the worker path). Documents the two-caller split.
WorkerNeverQuits == (mode = "worker") => (operator_quit = FALSE)

Safety ==
    /\ TypeOK
    /\ InteractiveStopYields
    /\ WorkerPathUnchanged
    /\ TurnBounded
    /\ WorkerNeverQuits

----------------------------------------------------------------------------
\* Liveness.

\* No-livelock: the busy region is always eventually exited. The harness
\* cannot spin forever inside sending/decoding/dispatching/yield — it always
\* returns to a quiescent state (awaiting the operator, or stopped).
NoLivelock == []<>(pc \in Quiescent)

\* No-deadlock-by-design: the ONLY terminal absorbing state is `stopped`
\* (intended). Hence CHECK_DEADLOCK is FALSE in the .cfg and no-livelock is
\* checked as a liveness property instead. Stated here for the reader: every
\* non-stopped state has a successor under Next (mechanically witnessed by
\* TLC finding no deadlock other than the intended `stopped`).

THEOREM SpecTypeOK    == Spec => []TypeOK
THEOREM SpecSafety    == Spec => []Safety
THEOREM SpecNoLivelock == Spec => NoLivelock
============================================================================
