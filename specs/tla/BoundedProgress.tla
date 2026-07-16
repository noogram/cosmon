-------------------------- MODULE BoundedProgress ----------------------------

(****************************************************************************
 * BoundedProgress.tla — Skeleton: liveness + bounded-progress property
 *
 * Imports CosmonRuntime (shared state, safety invariants).
 * Adds the liveness layer: the (Phi, FuelBudget) lexicographic well-founded
 * variant that proves no spin-without-progress.
 *
 * Property specified here (liveness):
 *   5. Bounded-Progress — every progress-capable molecule eventually reaches
 *      a terminal state (Completed or Collapsed), provided fuel is positive
 *      and weak fairness holds on the Next relation.
 *
 * Key constraint (Convergence 6, ADR-138 §3.4; amended by D4):
 *   Phi sums ONLY over {Pending, Queued, Running} — the forward-progress-capable
 *   statuses. Queued is in-domain (transient dispatch step; excluding it would
 *   let Queued -> Running increase Phi). Frozen and Starved contribute 0 to Phi
 *   — they are alive-and-stalled, not advancing; including them fires a spurious
 *   stall signal on every frozen-backlog tick.
 *
 * Dual constraint (delib-20260626-9825 D4 — the false-GREEN fix):
 *   Vitality is read off a SEPARATE live-work measure L, not off Phi. Phi = 0
 *   does NOT mean "healthy/quiescent": a fleet pinned at Starved (or stuck
 *   Queued) has Phi = 0 yet is alive-and-blocked. Reading the vitality light off
 *   Phi alone produces a false GREEN. Invariant: Phi = 0 /\ L > 0 => AMBER.
 *   Starved counts in L (externally-imposed block -> surface AMBER); Frozen is
 *   the documented contributes-0-to-L carve-out (operator-intended hold).
 *
 * Source: ADR-138 §3.4 + §8, delib-20260626-d222 synthesis (von-neumann +
 *   adversary), amended by delib-20260626-9825 D4 (vitality design).
 *
 * Convention: (* TODO *) marks every mechanical body a TLA+ author completes.
 ****************************************************************************)

EXTENDS CosmonRuntime, Naturals, FiniteSets

-----------------------------------------------------------------------------
(* §1. PHI — THE PROGRESS MEASURE *)
-----------------------------------------------------------------------------

\* Phi = Σ (steps_remaining(m) + 1) for all m in ProgressCapableMols.
\*
\* DOMAIN CONSTRAINT (mandatory correctness fix):
\*   Only {Pending, Queued, Running} molecules are summed.
\*   Frozen, Starved, Completed, Collapsed contribute 0.
\*   This is not optional — it is a correctness invariant, not a design choice.
\*   Queued is IN the domain: it is the transient assigned-but-not-executing
\*   dispatch step; excluding it would let the Queued -> Running transition
\*   INCREASE Phi, which a well-founded variant must never do.
\*   (Source: adversary finding in delib-20260626-d222; Queued/vitality gap
\*    closed in task-20260626-1743 / delib-20260626-9825 D4; verified against
\*    MoleculeStatus enum in cosmon-core/src/molecule.rs.)
\*
\* steps_remaining is a function mol_id -> Nat (supplied externally or derived
\* from formula total_steps - current_step).
Phi(steps_remaining) ==
    (* TODO: LET alive == { m \in molecules : m.status \in ProgressCapable }
     *       IN  Σ_{m \in alive} (steps_remaining[m.id] + 1)
     *)
    0

\* The Phi domain invariant — stated explicitly as a named property.
PhiDomainInvariant ==
    \* No status outside {Pending, Queued, Running} contributes to the progress
    \* measure. This ensures Phi = 0 iff no FORWARD-PROGRESS-CAPABLE work remains
    \* — NOT "iff no work is alive" (a starved/frozen backlog has Phi = 0 but is
    \* still alive; vitality is the separate L measure, see PhiZeroVitality).
    \A m \in molecules :
        m.status \notin ProgressCapable =>
            (* TODO: steps_remaining[m.id] does not appear in Phi *)
            TRUE

-----------------------------------------------------------------------------
(* §2. FUEL BUDGET *)
-----------------------------------------------------------------------------

\* FuelBudget(mol_id): the remaining retry budget for molecule mol_id.
\* When FuelBudget reaches 0, the runtime must Escalate (not retry silently).
\* This is the second element of the lexicographic variant.
FuelBudget(mol_id) == fuel[mol_id]

\* Every progress-capable molecule has positive fuel or is about to be escalated.
FuelPositiveOrEscalating ==
    \A m \in ProgressCapableMols :
        \/ FuelBudget(m.id) > 0
        \/ (* TODO: Escalate action is enabled for m.id *) TRUE

-----------------------------------------------------------------------------
(* §3. LEXICOGRAPHIC VARIANT — (Phi, FuelBudget) *)
-----------------------------------------------------------------------------

\* LexLess: (phi1, b1) < (phi2, b2) iff phi1 < phi2, or phi1 = phi2 /\ b1 < b2.
\* This is the well-founded order on N x N that guarantees termination.
LexLess(phi1, b1, phi2, b2) ==
    \/ phi1 < phi2
    \/ /\ phi1 = phi2
       /\ b1 < b2

\* The variant decreases on every step or retry.
\* Every Advance action decreases Phi by at least 1.
\* Every retry that does not advance Phi decreases FuelBudget by at least 1.
VariantDecreases ==
    (* TODO: \A mol_id \in { m.id : m \in ProgressCapableMols } :
     *         LexLess(Phi'(steps_remaining'), FuelBudget'(mol_id),
     *                 Phi(steps_remaining),  FuelBudget(mol_id))
     *)
    TRUE

-----------------------------------------------------------------------------
(* §4. WEAK FAIRNESS — liveness assumption *)
-----------------------------------------------------------------------------

\* Weak fairness on Next: if a progress action is continuously enabled, it
\* eventually fires.  This is the minimum liveness assumption for a single-loop
\* autonomous runtime with no adversarial environment.
WF_vars(A) == WF_vars(A)   \* placeholder — TLA+ author expands

Fairness == WF_vars(Next)

-----------------------------------------------------------------------------
(* §5. BOUNDED-PROGRESS PROPERTY *)
-----------------------------------------------------------------------------

\* Every molecule that is currently progress-capable eventually reaches a
\* terminal status (Completed or Collapsed).
\*
\* This is guaranteed by:
\*   (a) the (Phi, FuelBudget) variant decreasing on every tick (§3)
\*   (b) weak fairness on Next (§4)
\*   (c) the Phi domain restricted to {Pending, Queued, Running} (§1 domain constraint)
\*
\* Without (c), a frozen backlog keeps Phi > 0 indefinitely even when no
\* forward progress is possible — the property becomes vacuously false.
BoundedProgress ==
    \A m \in molecules :
        m.status \in ProgressCapable ~>
            m.status \in {"Completed", "Collapsed"}

\* Stall detection: if Phi is flat across k ticks, emit a signal.
\* This is the stall-witness obligation (ADR-137) expressed as a temporal property.
StallWitness(k) ==
    (* TODO: \neg (\exists k-tick window where Phi is unchanged
     *             /\ ProgressCapableMols is non-empty)
     *)
    TRUE

-----------------------------------------------------------------------------
(* §6. PHI DOMAIN EXCLUSION INVARIANT (load-bearing — MUST NOT be removed) *)
-----------------------------------------------------------------------------

\* This invariant is the formal statement of the mandatory correctness fix
\* from Convergence 6 (delib-20260626-d222) and ADR-138 §3.4.
\*
\* A model checker that finds this invariant false has discovered a Phi
\* implementation that includes Frozen/Starved molecules — that is a bug,
\* not a spec weakness.
\*
\* NOTE — excluding Starved from Phi is NOT the same as ignoring it. Starved
\* (and Queued) are still alive: they count in the live-work measure L and so
\* drive the vitality light (see §6b). The exclusion is purely about the
\* termination variant; the vitality reading is a separate measure.
PhiExcludesFrozenStarved ==
    \* Phi must be computable solely from {Pending, Queued, Running} molecules.
    \* The complement (Frozen, Starved, Completed, Collapsed) contributes nothing.
    \A m \in molecules :
        (m.status = "Frozen" \/ m.status = "Starved") =>
            (* TODO: m does not appear as a summand in Phi(steps_remaining) *)
            TRUE

-----------------------------------------------------------------------------
(* §6b. VITALITY INVARIANT — the false-GREEN fix (delib-20260626-9825 D4) *)
-----------------------------------------------------------------------------

\* The load-bearing dual of §6. Phi = 0 means "no forward-progress-capable
\* work", NOT "fleet healthy/quiescent". A fleet whose only alive molecules are
\* Starved (or stuck Queued) has Phi = 0 yet L > 0 — it is alive-and-blocked.
\* Reading the vitality light off Phi alone reports GREEN for such a fleet:
\* the false GREEN this invariant forbids.
\*
\* Vitality (defined in CosmonRuntime) is computed from (Phi, L):
\*   L = 0            -> GREEN  (quiescent — nothing alive)
\*   L > 0 /\ phi > 0 -> GREEN  (alive AND advancing)
\*   L > 0 /\ phi = 0 -> AMBER  (alive but not progressing)
\*
\* A model checker that finds this invariant false has discovered a vitality
\* reading that collapses Phi = 0 to GREEN while live work remains — the exact
\* bug task-20260626-1743 was filed to close.
PhiZeroVitality ==
    (phi = 0 /\ L > 0) => Vitality = "AMBER"

\* Companion sanity check: when the fleet is genuinely empty of live work, the
\* light is GREEN (an all-done / idle fleet is not an alarm).
QuiescentIsGreen ==
    (L = 0) => Vitality = "GREEN"

-----------------------------------------------------------------------------
(* §7. COMPOSITE LIVENESS SPEC *)
-----------------------------------------------------------------------------

LivenessSpec ==
    /\ Spec
    /\ Fairness

LivenessInvariant ==
    /\ Safety                      \* inherited from CosmonRuntime
    /\ PhiDomainInvariant
    /\ PhiExcludesFrozenStarved    \* load-bearing — see §6
    /\ PhiZeroVitality             \* false-GREEN fix — see §6b
    /\ QuiescentIsGreen            \* empty fleet is GREEN, not AMBER — see §6b
    /\ FuelPositiveOrEscalating

LivenessProperties ==
    /\ BoundedProgress
    /\ []LivenessInvariant

=============================================================================
