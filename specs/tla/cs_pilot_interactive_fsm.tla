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
    AllowSilentTerminate,\* BOOLEAN — enable the bug action to exhibit the counterexample
    MaxNudges,           \* cap on submit re-nudges the confirm loop may spend
    BoundedNudgeBudget   \* BOOLEAN — model the PRE-FIX confirm loop (see below)

ASSUME /\ MaxTurns         \in Nat /\ MaxTurns         > 0
       /\ MaxOperatorTurns \in Nat /\ MaxOperatorTurns > 0
       /\ MaxNudges        \in Nat /\ MaxNudges        > 0
       /\ AllowSilentTerminate \in BOOLEAN
       /\ BoundedNudgeBudget   \in BOOLEAN

----------------------------------------------------------------------------
\* "delivering" is the sub-protocol added for task-20260724-c014: the paste ->
\* submit handshake that used to sit BELOW this model's floor. See the
\* delivery-layer section further down for why its absence made the seal green
\* on a spec that could not see the bug.
States == { "delivering", "awaiting", "sending", "decoding", "dispatching",
            "yield", "stopped" }

\* The "busy" region: the harness is mid-computation, not quiescent.
\* No-livelock asserts the busy region is always eventually exited.
\* "delivering" is busy, never quiescent: a worker parked there has produced
\* nothing. That is what makes NoLivelock catch the paste-sans-submit hang.
BusyStates == { "delivering", "sending", "decoding", "dispatching", "yield" }
Quiescent  == { "awaiting", "stopped" }

----------------------------------------------------------------------------
VARIABLES
    mode,           \* "interactive" | "worker" — fixed at Init, never mutated
    pc,             \* current FSM state \in States
    operator_quit,  \* BOOLEAN — TRUE iff the operator issued /quit
    turn_count,     \* 0..MaxTurns — model<->tool round-trips in the current turn
    op_turns,       \* 0..MaxOperatorTurns — operator turns served (forensic clock)
    delivery,       \* briefing-delivery phase — see the delivery layer below
    tui_ready,      \* BOOLEAN — is the TUI composer accepting a submit right now?
    nudges          \* 0..MaxNudges — submit re-nudges spent by the confirm loop

\* Grouped so every pre-existing action can hold the delivery layer fixed with
\* one conjunct, which keeps the original FSM text readable.
delivery_vars == << delivery, tui_ready, nudges >>

vars == << mode, pc, operator_quit, turn_count, op_turns, delivery, tui_ready,
           nudges >>

----------------------------------------------------------------------------
\* Init — the interactive REPL starts awaiting the operator (a human types at
\* the composer, so there is no paste handshake to model: delivery is already
\* "submitted"). The one-shot worker now starts one floor LOWER than it used
\* to: at "delivering", with its briefing pasted into the composer but NOT yet
\* submitted.
\*
\* The old Init asserted `pc = "sending"` for a worker — "the worker starts
\* already sending its immutable briefing to the model". That assumption is
\* exactly the thing that breaks in the field, so the reachable-bad-state
\* "pasted-but-not-submitted -> idle forever" was not in the state space and
\* TLC could not see it (task-20260724-c014).
\*
\* The TUI's initial readiness is chosen non-deterministically: a fresh Claude
\* Code worker may be idle at the composer, or still busy with MCP-server auth
\* and startup rendering.

Init ==
    /\ mode \in { "interactive", "worker" }
    /\ pc = IF mode = "interactive" THEN "awaiting" ELSE "delivering"
    /\ operator_quit = FALSE
    /\ turn_count = 0
    /\ op_turns = 0
    /\ delivery = IF mode = "interactive" THEN "submitted" ELSE "pasted"
    /\ tui_ready \in BOOLEAN
    /\ nudges = 0

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
    /\ UNCHANGED delivery_vars

\* OperatorQuit — the `/quit` pilot directive. This is the ONLY way an
\* interactive session legitimately reaches `stopped`, and the ONLY action
\* that sets operator_quit. (A model Turn::Stop must never do this.)
OperatorQuit ==
    /\ mode = "interactive"
    /\ pc = "awaiting"
    /\ pc' = "stopped"
    /\ operator_quit' = TRUE
    /\ UNCHANGED << mode, turn_count, op_turns >>
    /\ UNCHANGED delivery_vars

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
    /\ UNCHANGED delivery_vars

\* BudgetExhausted — the per-turn budget is spent and the model still wants
\* to continue (HarnessError::TurnBudgetExhausted). Crucially, in interactive
\* mode this YIELDS to the operator (never silently stops); in worker mode it
\* terminates with the harness error.
BudgetExhausted ==
    /\ pc = "sending"
    /\ turn_count >= MaxTurns
    /\ pc' = IF mode = "interactive" THEN "yield" ELSE "stopped"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>
    /\ UNCHANGED delivery_vars

\* DecodeToolCalls — the decoded Turn is Turn::ToolCalls.
DecodeToolCalls ==
    /\ pc = "decoding"
    /\ pc' = "dispatching"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>
    /\ UNCHANGED delivery_vars

\* DecodeStop — the decoded Turn is Turn::Stop. THE load-bearing branch:
\* interactive -> yield to operator ; worker -> terminate.
DecodeStop ==
    /\ pc = "decoding"
    /\ pc' = IF mode = "interactive" THEN "yield" ELSE "stopped"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>
    /\ UNCHANGED delivery_vars

\* DispatchTool — execute the tool call, append the result, feed it back to
\* the model. A failed tool is recovered (loop continues), never aborts —
\* same shape as today's run_loop.
DispatchTool ==
    /\ pc = "dispatching"
    /\ pc' = "sending"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>
    /\ UNCHANGED delivery_vars

\* YieldToOperator — render the synthesis and HAND CONTROL BACK. This is the
\* transition that makes the interactive session a loop instead of a
\* one-shot: it returns to "awaiting", never to "stopped".
YieldToOperator ==
    /\ mode = "interactive"
    /\ pc = "yield"
    /\ pc' = "awaiting"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns >>
    /\ UNCHANGED delivery_vars

----------------------------------------------------------------------------
\* THE DELIVERY LAYER (task-20260724-c014) — the paste -> submit handshake.
\*
\* `cs tackle` pastes the briefing into a live Claude Code TUI, then presses
\* submit. The submit is fire-and-forget: a busy TUI drops the keypress
\* silently. `confirm_briefing_submitted` therefore polls the composer and
\* re-presses. The field defect is not a wrong keystroke — it is a BOUNDED
\* retry loop against an environment with UNBOUNDED delay.
\*
\* The environment is modelled adversarially and WITHOUT fairness: `tui_ready`
\* may stay FALSE for an arbitrary, unbounded number of ticks (MCP-server auth,
\* a loaded machine, a large multi-block paste re-rendering). Nothing in the
\* spec promises it ever becomes ready. That is the whole point: a design that
\* relies on the TUI settling inside a fixed window is unsound, and TLC must
\* say so.

\* The TUI settles. Deliberately UNFAIR — it may never happen.
TuiBecomesReady ==
    /\ ~tui_ready
    /\ tui_ready' = TRUE
    /\ UNCHANGED << mode, pc, operator_quit, turn_count, op_turns, delivery,
                    nudges >>

\* The TUI goes busy again (a repaint, a tool spinner). Also unfair.
TuiBusy ==
    /\ tui_ready
    /\ tui_ready' = FALSE
    /\ UNCHANGED << mode, pc, operator_quit, turn_count, op_turns, delivery,
                    nudges >>

\* The submit keystroke lands: the composer was ready. The briefing leaves the
\* input box and the worker starts sending to the model — i.e. the FSM enters
\* at the state the OLD Init simply assumed.
SubmitLands ==
    /\ delivery = "pasted"
    /\ tui_ready
    /\ delivery' = "submitted"
    /\ pc' = "sending"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns, tui_ready,
                    nudges >>

\* The submit keystroke is swallowed by a busy TUI and one retry is spent.
\* Nothing observable changes except the budget — this is the tick that, in the
\* field, repeated until the 90 s window expired.
SubmitSwallowed ==
    /\ delivery = "pasted"
    /\ ~tui_ready
    /\ nudges < MaxNudges
    /\ nudges' = nudges + 1
    /\ UNCHANGED << mode, pc, operator_quit, turn_count, op_turns, delivery,
                    tui_ready >>

\* THE FIX: a spent budget escalates to a TYPED failure the runtime acts on
\* (torn down, re-dispatchable). Enabled only when the confirm loop is NOT the
\* pre-fix bounded-then-silent one.
EscalateStuckBriefing ==
    /\ ~BoundedNudgeBudget
    /\ delivery = "pasted"
    /\ nudges >= MaxNudges
    /\ delivery' = "escalated"
    /\ pc' = "stopped"
    /\ UNCHANGED << mode, operator_quit, turn_count, op_turns, tui_ready,
                    nudges >>

\* THE PRE-FIX BUG, dormant unless BoundedNudgeBudget is set: the confirm loop
\* exhausts its window, emits a `warn!`, and RETURNS. Nobody ever presses
\* submit again — so even when the TUI finally settles, the briefing stays in
\* the composer. "abandoned" is absorbing, which is faithful to the field
\* observation that a manual submit twenty minutes later started the worker
\* instantly: the environment had recovered; the software had stopped trying.
GiveUpSilently ==
    /\ BoundedNudgeBudget
    /\ delivery = "pasted"
    /\ nudges >= MaxNudges
    /\ delivery' = "abandoned"
    /\ UNCHANGED << mode, pc, operator_quit, turn_count, op_turns, tui_ready,
                    nudges >>

\* The confirm loop's own step. Always enabled while the briefing is pasted
\* (land, or swallow-and-retry, or resolve the spent budget), so weak fairness
\* on the GROUP means the loop always makes progress even when the adversary
\* flickers `tui_ready` — which a per-action WF could not guarantee.
Delivery ==
    \/ SubmitLands
    \/ SubmitSwallowed
    \/ EscalateStuckBriefing
    \/ GiveUpSilently

Environment ==
    \/ TuiBecomesReady
    \/ TuiBusy

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
    /\ UNCHANGED delivery_vars

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
    \/ Delivery
    \/ Environment

\* Fairness on `Delivery` is the modelled promise that the confirm loop keeps
\* taking steps. `Environment` is deliberately absent: the TUI is under no
\* obligation to ever become ready.
Spec == Init /\ [][Next]_vars /\ WF_vars(Internal) /\ WF_vars(Delivery)

----------------------------------------------------------------------------
\* Invariants.

TypeOK ==
    /\ mode \in { "interactive", "worker" }
    /\ pc \in States
    /\ operator_quit \in BOOLEAN
    /\ turn_count \in 0..MaxTurns
    /\ op_turns \in 0..MaxOperatorTurns
    /\ delivery \in { "pasted", "submitted", "escalated", "abandoned" }
    /\ tui_ready \in BOOLEAN
    /\ nudges \in 0..MaxNudges

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

\* THE load-bearing delivery invariant (task-20260724-c014). A briefing is
\* never abandoned in the composer: the confirm loop either gets it submitted
\* or escalates a typed failure. "abandoned" is reachable ONLY through
\* GiveUpSilently, i.e. only under BoundedNudgeBudget — so this invariant is
\* the crisp statement of what the fix buys, and flipping the constant makes
\* TLC print its counterexample.
NoSilentAbandon == delivery # "abandoned"

\* A worker cannot be executing a briefing it never submitted. Rules out the
\* observed phantom: a `running` molecule, an empty worktree, a 0-byte
\* worker.stderr, and a pane sitting on `❯ [Pasted text #1 +NN lines]`.
NoWorkBeforeSubmit ==
    (pc \in { "sending", "decoding", "dispatching" }) => (delivery # "pasted")

\* The delivery handshake is worker-only: a human at an interactive composer
\* types their turn, there is no paste to submit on their behalf.
InteractiveSkipsDelivery ==
    (mode = "interactive") => (delivery = "submitted")

Safety ==
    /\ TypeOK
    /\ InteractiveStopYields
    /\ WorkerPathUnchanged
    /\ TurnBounded
    /\ WorkerNeverQuits
    /\ NoSilentAbandon
    /\ NoWorkBeforeSubmit
    /\ InteractiveSkipsDelivery

----------------------------------------------------------------------------
\* Liveness.

\* No-livelock: the busy region is always eventually exited. The harness
\* cannot spin forever inside sending/decoding/dispatching/yield — it always
\* returns to a quiescent state (awaiting the operator, or stopped).
NoLivelock == []<>(pc \in Quiescent)

\* THE delivery liveness (task-20260724-c014). A pasted briefing always
\* eventually reaches a resolved delivery — submitted, or escalated as a typed
\* failure. Never "pasted forever".
\*
\* This is the property the pre-fix design cannot satisfy, and it fails for the
\* right reason: the adversary is allowed to hold `tui_ready` FALSE forever, so
\* no fixed retry window can be enough; only escalation resolves the state.
\* Weak fairness on `Delivery` supplies the "the loop keeps trying" half, which
\* is what makes the counterexample about the DESIGN and not about a scheduler
\* that simply stopped running the loop.
BriefingResolves ==
    (delivery = "pasted") ~> (delivery \in { "submitted", "escalated" })

\* No-deadlock-by-design: the ONLY terminal absorbing state is `stopped`
\* (intended). Hence CHECK_DEADLOCK is FALSE in the .cfg and no-livelock is
\* checked as a liveness property instead. Stated here for the reader: every
\* non-stopped state has a successor under Next (mechanically witnessed by
\* TLC finding no deadlock other than the intended `stopped`).

THEOREM SpecTypeOK    == Spec => []TypeOK
THEOREM SpecSafety    == Spec => []Safety
THEOREM SpecNoLivelock == Spec => NoLivelock
THEOREM SpecBriefingResolves == Spec => BriefingResolves
============================================================================
