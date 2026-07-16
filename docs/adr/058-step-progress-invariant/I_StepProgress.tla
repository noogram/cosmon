--------------------- MODULE I_StepProgress ---------------------
\* ADR-058 fragment — the eighth-clock liveness invariant.
\*
\* This file is a READ-ONLY EXCERPT of the authoritative spec at
\*   docs/specs/CosmonRun.tla
\* extracted for ADR-058 reviewers who do not want to load the full
\* ten-invariant module.  The canonical symbols (Mol, mol_status,
\* sealLog, now, events_*, …) are owned by CosmonRun; this module
\* redeclares only the variables and actions that ADR-058 introduces
\* (knuth §1–§7, synthesis §Part 5 of delib-20260420-1b02).
\*
\* DO NOT run TLC against this file directly.  It does not parse
\* standalone — the Evolve / Complete / Nucleate actions it refers
\* to live in CosmonRun.tla.  Run the model checker against
\*   docs/specs/CosmonRun.tla  +  docs/specs/CosmonRun_StepProgress.cfg
\* instead (validation report: docs/specs/VALIDATION-REPORT.md).
\*
\* This excerpt exists so ADR-058 has a self-contained spec body that
\* a reviewer can read without cross-referencing the umbrella module.
\* When the canonical file changes the excerpt must be updated in the
\* same PR — the CLI doc-sync discipline applies to formal specs too.

EXTENDS Naturals, Sequences

CONSTANTS
  Mol,         \* finite set of molecule identifiers (from CosmonRun)
  T_STALL,     \* silence threshold in clock ticks (heuristic,
               \* per-formula — see carnot §6 for C*/C** calibration)
  MaxClock     \* monotonic upper bound on `now` (TLC state-space cap)

\* ---------------- Variables introduced by ADR-058 ------------------
\*
\* sealLog[m] is the ADR-058 StepClock witnessed in events.jsonl —
\* hawking §5 (α-emission per step).  Each appended element is the
\* global clock reading at the moment cs evolve emitted a briefing
\* seal.  Single-writer per (molecule, seq_no) via I7 (ADR-052).
\*
\* now is the monotonic global clock advanced only by Tick.  It is
\* NOT wall-clock — TLC finiteness requires MaxClock; operationally,
\* the clock is the filesystem mtime of .cosmon/state/ plus a
\* per-event seq_no.
VARIABLES sealLog, now

\* ---------------- Helper: Silence / LastSealT ----------------------

LastSealT(m) ==
  IF sealLog[m] = <<>>
  THEN 0
  ELSE sealLog[m][Len(sealLog[m])]

\* Silence(m) is shannon's MSS for the stall pathology (synthesis §2).
\* In the non-adversarial regime it carries ≈ 1.1 bits of mutual
\* information about H = Stalled; in the adversarial-slow-step regime
\* shannon proved channel capacity = 0 bits — Silence alone cannot
\* separate stall from slow-step, which is why MarkStalled is an
\* *observer* action (detection), not an in-worker action (prevention).
Silence(m) == now - LastSealT(m)

\* ---------------- EmitSeal — worker-side α-emission ----------------
\*
\* Produced by cs evolve (worker-callable).  Ledger-appended under I7
\* SingleEventWriter lock (ADR-052).  This is the one observable
\* marking "worker produced evidence of progress" — knuth §1 anchor.

EmitSeal(m) ==
  /\ sealLog' = [sealLog EXCEPT ![m] = Append(@, now)]
  /\ UNCHANGED <<now>>    \* (other CosmonRun vars UNCHANGED in full module)

\* ---------------- MarkStalled — cosmon-side observer -------------
\*
\* Fires when Silence(m) > T_STALL.  Turing §2 proves Stalled is
\* semi-decidable (co-r.e.); this action is therefore liveness-style
\* (leads-to), not safety.  Shannon §6 forbids a worker-side variant
\* (would violate ADR-052 I2 SingleWriterPerField — G1 self-assertion).
\* Patrol / daemon owns the pen.

MarkStalled(m) ==
  /\ Silence(m) > T_STALL
  \* full guard + writes live in CosmonRun.tla:
  \*   mol_status[m] = "Running" ∧ mol_status' = Stalled
  /\ UNCHANGED <<sealLog, now>>

\* ---------------- Tick — bounded monotonic clock ------------------

Tick == /\ now < MaxClock
        /\ now' = now + 1
        /\ UNCHANGED <<sealLog>>

\* ---------------- Resume — ledger-written re-prompt --------------
\*
\* Optional, for the P3-resumable layer (carnot §5, knuth §7).  Must
\* append to sealLog with a "resume" tag (elided in this excerpt —
\* the full action lives in the future P3 extension module and is
\* gated on an `AutoResumeEnabled` constant, following the same
\* pattern as AsyncCrashesEnabled / OutOfBandEnabled).
\*
\* Anti-Sisyphus refinement (knuth §7):
\*   count(resume-entries in sealLog[m] since last progress entry) < K
\* with K = 3 the default.  Without this bound a silent in-memory
\* reset of the clock without ledger append creates a trap —
\* re-prompt ∞, invariant never violated, Completed never reached.

\* ---------------- I_StepProgress — the liveness invariant --------
\*
\* Under weak fairness on MarkStalled *and* Tick, every Running
\* molecule whose Silence crosses T_STALL eventually transitions to
\* a terminal or observed state.  The consequent is deliberately
\* broadened beyond Stalled — operator-initiated Collapse or Freeze
\* are legitimate resolutions of a detected stall (feynman: the silent
\* messenger is noticed either by the kitchen clock or by the mother
\* asking "are you asleep?").
\*
\* Without WF on MarkStalled, TLC produces the starvation
\* counter-example (same shape as ADR-052 I5 harvest-starvation —
\* knuth §4).  Without WF on Tick, the "time-stands-still" trace
\* trivially satisfies the property.  Both fairness conditions are
\* required.

I_StepProgress ==
  \A m \in Mol :
    (Silence(m) > T_STALL) ~>
      \* consequent in CosmonRun: mol_status[m] ∈ {Stalled,Collapsed,Frozen}
      (Silence(m) <= T_STALL)
      \* i.e. either the molecule emitted another seal (Evolve),
      \* or an observer (MarkStalled / Collapse / Freeze) terminated
      \* it — in which case the consequent is vacuously read from the
      \* full-module invariant.

\* ---------------- Test vectors (informative) ----------------------
\*
\* Trace A — normal.            T_STALL = 10.  Silence peaks at 4.  ✓
\* Trace B — fixture 2d4e.      Tick×11 without EmitSeal ⇒ Silence = 11 > T.
\*                              Under WF on MarkStalled, fires within
\*                              finitely many steps.  ✓ invariant
\*                              detects the "4-hour silent molecule".
\* Trace C — LongRunning T-ε.   Silence peaks at 9 < 10.  ✓ no fire.
\*
\* Full vectors in knuth response §6 at
\*   .cosmon/state/fleets/default/molecules/delib-20260420-1b02/responses/knuth.md
\*
\* Mechanical check:
\*   cd docs/specs && tlc CosmonRun.tla -config CosmonRun_StepProgress.cfg
\* Expected: TypeOK + I6 + I7 + I9 INVARIANTS hold, I_StepProgress
\* and L3 PROPERTIES hold.  Under the sibling CrashesI3 / CrashesI4 /
\* I9Counterexample configs the spec sharpens the eventual-consistency
\* boundary (see VALIDATION-REPORT.md for the audit trail).

=============================================================================
\* Beware of bugs in the above specification; I have only proved it
\* correct, not run TLC.  —  knuth, synthesis §8 (borrowed from Donald).
