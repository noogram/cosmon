-------------------------- MODULE CosmonRuntime ----------------------------

(****************************************************************************
 * CosmonRuntime.tla — Skeleton: shared state + safety properties
 *
 * Specifies the cosmon autonomous runtime at the level of molecule lifecycle,
 * authority tokens, and the four RR-5 forensic events.
 *
 * Properties specified here (all safety):
 *   1. Idempotent-Replay    — applying a Completed step twice = once
 *   2. Crash-Recovery       — after any crash, state is consistent with
 *                             the last Completed step
 *   3. Regime-Boundary      — the Runtime holds no DoneToken(Human)
 *   4. Merge-Before-Dispatch — a dependent is never dispatched before its
 *                             blocker's merge event is recorded
 *
 * Companion: BoundedProgress.tla (liveness; imports this module).
 *
 * Source: ADR-138 §8, delib-20260626-d222 synthesis (von-neumann lead).
 * MSRV reference: cosmon MSRV 1.88 (ADR-065).
 *
 * Convention: (* TODO *) marks every mechanical body a TLA+ author completes.
 *             State variables are declared; key invariant formulas are sketched.
 ****************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets

-----------------------------------------------------------------------------
(* §1. STATUS DOMAIN *)
-----------------------------------------------------------------------------

\* The seven molecule statuses in cosmon-core.
\* Source: MoleculeStatus enum in cosmon-core/src/molecule.rs
\* "Queued" (assigned-to-a-worker-but-not-yet-executing) was previously absent
\* from this set — that omission was the gap that left the Phi/vitality
\* classification incomplete (task-20260626-1743, delib-20260626-9825 D4).
Status == {"Pending", "Queued", "Running", "Frozen", "Starved", "Completed", "Collapsed"}

\* Progress-capable statuses — the Phi domain (Convergence 6, ADR-138 §3.4,
\* amended by delib-20260626-9825 D4).
\* Phi sums over {Pending, Queued, Running}: the forward-progress-capable
\* statuses. Queued belongs here because it is the transient dispatch step
\* Pending -> Queued -> Running; excluding it would let Queued -> Running
\* INCREASE Phi, breaking the well-founded variant. Frozen and Starved are
\* alive-and-stalled (not about to advance) and so contribute 0 to Phi.
ProgressCapable == {"Pending", "Queued", "Running"}

\* Live-work statuses — the vitality (L) domain. DISTINCT from ProgressCapable.
\* L answers "is there alive work at all?", not "can it advance now?".
\* Starved is included (externally-imposed block, ADR-062 — the operator may
\* not know, so it MUST surface as AMBER). Frozen is the documented carve-out:
\* it is an operator-INTENDED hold (cs freeze), so surfacing AMBER would be
\* noise — it contributes 0 to the vitality signal by design, not by accident.
\* The false-GREEN bug (Phi=0 read as "healthy/quiescent" on a starved fleet)
\* is killed by reading vitality off L, not off Phi (see Vitality below).
LiveWork == {"Pending", "Queued", "Running", "Starved"}

\* Authority domain: Human or Runtime.
Authority == {"Human", "Runtime"}

-----------------------------------------------------------------------------
(* §2. STATE VARIABLES *)
-----------------------------------------------------------------------------

VARIABLES
    molecules,
    \* The fleet: a set of molecule records.
    \* Each record: [ id : STRING, status : Status, step : Nat ]
    \* Invariant: step = 0 for Pending, >= 1 for Running/Completed.

    done_tokens,
    \* Set of authority-tagged done tokens: { [ mol_id : STRING, authority : Authority ] }
    \* A DoneToken(Human) is required to call cs done from a human caller.
    \* A DoneToken(Runtime) is what the autonomous loop may hold.
    \* Regime-Boundary: done_tokens contains no { authority = "Runtime" } entry
    \* that would authorize calling cs done (see §5.3).

    fuel,
    \* fuel : STRING -> Nat
    \* Maps each molecule id to its remaining FuelBudget.
    \* A molecule with fuel[id] = 0 triggers Escalate(id).

    phi
    \* phi : Nat
    \* The current value of Phi = Σ (steps_remaining + 1) over ProgressCapable molecules.
    \* Declared here; the invariant that phi > 0 implies progress-capable molecules
    \* exist is the Bounded-Progress liveness property (see BoundedProgress.tla).

vars == << molecules, done_tokens, fuel, phi >>

-----------------------------------------------------------------------------
(* §3. TYPE INVARIANT *)
-----------------------------------------------------------------------------

TypeOK ==
    /\ molecules \subseteq [id : STRING, status : Status, step : Nat]
    /\ done_tokens \subseteq [mol_id : STRING, authority : Authority]
    /\ \A id \in DOMAIN fuel : fuel[id] \in Nat
    /\ phi \in Nat

-----------------------------------------------------------------------------
(* §4. HELPER OPERATORS *)
-----------------------------------------------------------------------------

\* Molecules in a given status.
MolsInStatus(s) == { m \in molecules : m.status = s }

\* Progress-capable subset (the Phi domain).
ProgressCapableMols == { m \in molecules : m.status \in ProgressCapable }

\* Live-work subset (the vitality / L domain) — alive-but-not-terminal work
\* that the operator still owns, whether or not it can advance this tick.
LiveWorkMols == { m \in molecules : m.status \in LiveWork }

\* Compute Phi from the current fleet (sum of distances over progress-capable mols).
\* steps_remaining is a function mol -> Nat supplied by the formula spec.
PhiOf(step_remaining_fn) ==
    (* TODO: sum (step_remaining_fn[m.id] + 1) for m in ProgressCapableMols *)
    0

\* The live-work measure L = count of alive-but-not-terminal molecules.
\* Drives the vitality light; DISTINCT from Phi (ADR-138 §3.4, D4).
L == Cardinality(LiveWorkMols)

\* Vitality light — three states, computed from (Phi, L), NOT from Phi alone.
\*   L = 0            -> "GREEN"  (quiescent: nothing alive, legitimately idle/done)
\*   L > 0 /\ phi > 0 -> "GREEN"  (alive AND advancing)
\*   L > 0 /\ phi = 0 -> "AMBER"  (alive but nothing forward-progress-capable:
\*                                 the starved / queued-stuck fleet that used to
\*                                 read false-GREEN)
\* (RED — dead/zombie Running — is the ADR-116 health-witness scope, not modelled
\*  by this variant; named for completeness.)
Vitality ==
    IF L = 0 THEN "GREEN"
    ELSE IF phi > 0 THEN "GREEN"
    ELSE "AMBER"

\* A done token exists for a molecule with the given authority.
HasToken(mol_id, auth) ==
    \E t \in done_tokens : t.mol_id = mol_id /\ t.authority = auth

-----------------------------------------------------------------------------
(* §5. SAFETY PROPERTIES *)
-----------------------------------------------------------------------------

(* §5.1 — Idempotent-Replay
 *
 * Applying a Completed step twice leaves state unchanged.
 * Formally: if a molecule m is Completed, any action that re-applies its
 * terminal step must be a no-op on the molecules set.
 *)
IdempotentReplay ==
    \A m \in molecules :
        m.status = "Completed" =>
            (* TODO: no action can decrement m.step or change m.status away from Completed *)
            TRUE

(* §5.2 — Crash-Recovery
 *
 * After any crash (modelled as an arbitrary state revert), the reconstructed
 * state is consistent with the last Completed step recorded in events.jsonl.
 * In TLA+: if a molecule m is Completed, there exists a step k such that
 * m.step = k and no subsequent Completed record contradicts k.
 *
 * This is cosmon's "resume from last good step" guarantee — distinct from
 * deterministic forensic replay (which requires recording completion bytes;
 * see ADR-138 §8, Phase 0 Divergence B).
 *)
CrashRecovery ==
    (* TODO: \forall m \in MolsInStatus("Completed") :
     *         \E k \in 1..m.step : last_sealed_step[m.id] = k
     *)
    TRUE

(* §5.3 — Regime-Boundary
 *
 * The Runtime holds no DoneToken(Human).  Equivalently: there is no done
 * token with authority = "Human" that was issued to the runtime process.
 *
 * In the Rust typestate model this is inexpressible as a runtime action —
 * the state "Runtime holds DoneToken<Human>" does not exist at the type level.
 * In TLA+ we express it as an invariant on done_tokens.
 *
 * Note: this invariant covers the in-process boundary.  The out-of-process
 * gate is the cs done perimeter + RR-5 RuntimeMergeDispatched event
 * (ADR-138 §3.5, §4).
 *)
RegimeBoundary ==
    (* The runtime process identifier is modelled as a special constant.
     * In practice: no DoneToken exists that would let the runtime call cs done
     * in the Human-authorized code path.
     *)
    (* TODO: \neg HasToken("__runtime__", "Human") *)
    TRUE

(* §5.4 — Merge-Before-Dispatch
 *
 * A dependent molecule d is never dispatched (status moves from Pending to
 * Running) unless all its blockers have recorded a RuntimeMergeDispatched
 * event (i.e., their done is committed to the event log).
 *
 * This is the RR-5 prerequisite woven into the safety spec.
 *)
MergeBeforeDispatch ==
    (* TODO: \forall d \in MolsInStatus("Running") :
     *         \forall blocker_id \in blocked_by[d.id] :
     *             \E m \in molecules : m.id = blocker_id /\ m.status = "Completed"
     *)
    TRUE

-----------------------------------------------------------------------------
(* §6. COMPOSITE SAFETY INVARIANT *)
-----------------------------------------------------------------------------

Safety ==
    /\ TypeOK
    /\ IdempotentReplay
    /\ CrashRecovery
    /\ RegimeBoundary
    /\ MergeBeforeDispatch

-----------------------------------------------------------------------------
(* §7. INITIAL STATE *)
-----------------------------------------------------------------------------

Init ==
    (* TODO: define starting fleet, empty tokens, full fuel, phi = 0 *)
    /\ molecules = {}
    /\ done_tokens = {}
    /\ fuel = [ x \in {} |-> 0 ]
    /\ phi = 0

-----------------------------------------------------------------------------
(* §8. ACTIONS (stubs) *)
-----------------------------------------------------------------------------

\* Dispatch: move a Pending molecule to Running (cs tackle).
Dispatch(mol_id) ==
    (* TODO: precondition — mol is Pending, all blockers Completed,
     *       postcondition — mol.status = "Running", fuel unchanged *)
    UNCHANGED vars

\* Advance: increment step (cs evolve).
Advance(mol_id) ==
    (* TODO: precondition — mol is Running, StepVerdict = Pass,
     *       postcondition — mol.step incremented, phi decremented *)
    UNCHANGED vars

\* Complete: Running -> Completed (cs complete).
Complete(mol_id) ==
    (* TODO: precondition — mol is Running, all steps done,
     *       postcondition — mol.status = "Completed", emit RuntimeMergeDispatched *)
    UNCHANGED vars

\* Collapse: Running/Pending -> Collapsed (cs collapse).
Collapse(mol_id, reason) ==
    (* TODO *)
    UNCHANGED vars

\* Freeze: Running/Pending -> Frozen (cs freeze).
Freeze(mol_id) ==
    (* TODO *)
    UNCHANGED vars

\* Escalate: emit signal molecule, park as temp:frozen.
Escalate(mol_id) ==
    (* TODO: fuel[mol_id] = 0 precondition *)
    UNCHANGED vars

-----------------------------------------------------------------------------
(* §9. NEXT-STATE RELATION *)
-----------------------------------------------------------------------------

Next ==
    \/ \E id \in { m.id : m \in molecules } :
        \/ Dispatch(id)
        \/ Advance(id)
        \/ Complete(id)
        \/ Collapse(id, "policy-resolved")
        \/ Freeze(id)
        \/ Escalate(id)

-----------------------------------------------------------------------------
(* §10. SPEC *)
-----------------------------------------------------------------------------

Spec == Init /\ [][Next]_vars

=============================================================================
