-------------------------------- MODULE spore --------------------------------
\* ==========================================================================
\* spore.tla — the TLC-checked seal for the `cosmon-dev` spore.
\*
\* WHAT THIS MODULE IS
\*   A spore declares a `[spore.seal]` that NAMES safety properties of the whole
\*   polymer it germinates. This module is the mechanical proof the seal stands
\*   for: it models the germinated DAG's gate semantics + the bounded convergence
\*   loop and lets TLC discharge the four properties the seal claims.
\*
\* WHAT IS MODELLED
\*   * the gate DAG (blueprint §3, diamond topology) as a finite set of nodes with
\*     a blocked-by dependency relation lifted verbatim from spore.toml:
\*       trace (root+leaf), intake, contract, reproduce, falsify, implement, green,
\*       ci_gate, converge (the §6bis loop node), rehearsal, dissent, release,
\*       confirm;
\*   * node drainage — a Pending node executes once every dependency is Done;
\*   * the §6bis CONVERGENCE LOOP as a BOUNDED round counter (0..MaxRounds): each
\*     round the two review engines (claude, codex-sol) return a verdict that MAY
\*     be absent (a completed seat that emitted no verdict.json — the exact
\*     silent-degrade the gate must survive). The loop reaches a CLEAN fixpoint
\*     only when BOTH engines are CLEAN in the same round; at MaxRounds it BLOCKS
\*     (escalation), never silently passes;
\*   * the fail-closed gate verdicts — every gate reads evidence that MAY be
\*     absent, and absence REFUSES;
\*   * artifact writes — each node writes one path; convergence rounds carry the
\*     round index (the load-bearing detail NoResourceCollision guards).
\*
\* WHAT IS NOT MODELLED (honest boundary)
\*   * review/patch CONTENT (Rice: truth of a string is undecidable) — the model
\*     tracks only whether a machine verdict is PRESENT and what it says;
\*   * LLM agent semantics (a non-deterministic oracle beyond TLA+'s reach) —
\*     abstracted as the non-deterministic choice of each engine's verdict;
\*   * filesystem races / worktree lifecycle (worktree isolation is assumed, and
\*     is exactly what makes the static path-injectivity argument sound);
\*   * human controls (identities, credentials, branch-protection) — the spore
\*     germs the topology, not the human independence (blueprint §8 limite dure).
\*
\* THE FOUR PROPERTIES (the seal's `properties = [...]`)
\*   Termination                  — every germinated polymer either DRAINS (every
\*                                  node Done) or reaches the typed `blocked`
\*                                  escalation at MaxRounds. The gate DAG is acyclic
\*                                  and the convergence loop is bounded by MaxRounds,
\*                                  so no cycle and no unbounded foaming. No spin.
\*   GateFailClosed  (LOAD-BEARING) — no gate promotes on absent/failing evidence:
\*                                  a gate BLOCKS on a missing upstream verdict, the
\*                                  convergence yields CLEAN only when BOTH engines
\*                                  are CLEAN, release SHIPs only when every upstream
\*                                  gate PASSED, the loop is CLEAN, and the dissent
\*                                  field is non-empty. Absence refuses, always.
\*   DeterministicParametrization — the node set is a pure function of the params;
\*                                  nothing environmental perturbs the expansion.
\*   NoResourceCollision          — no two nodes (nor two convergence rounds) write
\*                                  the same artifact path (the round index makes
\*                                  round-1 disjoint from round-2).
\*
\* Modeled as a bounded finite-state pipeline (drainage to an absorbing terminal
\* state, or a bounded loop to a CLEAN fixpoint / BLOCKED escalation). Launch
\* command + expected verdict live in spore.cfg.
\* ==========================================================================

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    MaxRounds       \* the `max_rounds` param: a positive integer bound on the
                    \* §6bis convergence loop (the foaming variant Termination bounds)

ASSUME MaxRounds \in Nat /\ MaxRounds >= 1

\* --------------------------------------------------------------------------
\* Node identities — the expansion of the DAG (DeterministicParametrization).
\* Every node is fixed (germinates exactly one molecule); the ONLY emergent node,
\* `converge`, is modelled as a single control node PLUS an internal bounded round
\* counter (its emergent children are the rounds, bounded by MaxRounds). The node
\* SET is param-independent: risk / review_scale / max_rounds shape TOPICS and the
\* loop BOUND, never the node set (blueprint §8).
\* --------------------------------------------------------------------------
Roles ==
    { "trace", "intake", "contract", "reproduce", "falsify", "implement",
      "green", "ci_gate", "converge", "rehearsal", "dissent", "release",
      "confirm" }

\* --------------------------------------------------------------------------
\* Dependency relation — the blocked-by edges of spore.toml, verbatim.
\* DIAMOND (blueprint §3): reproduce forks into implement (fix arm) and falsify
\* (falsification arm); they reconverge at green. converge forks into rehearsal
\* and dissent; they reconverge at release.
\*   intake -> contract -> reproduce ─┬─> implement ─┐
\*                                     └─> falsify ───┴─> green -> ci_gate
\*                                                             -> converge ─┬─> rehearsal ─┐
\*                                                                          └─> dissent ───┴─> release -> confirm
\*   trace: ROOT+LEAF (no deps, nothing depends on it).
\* --------------------------------------------------------------------------
Deps(r) ==
    CASE r = "trace"     -> {}
      [] r = "intake"    -> {}
      [] r = "contract"  -> { "intake" }
      [] r = "reproduce" -> { "contract" }
      [] r = "falsify"   -> { "reproduce" }
      [] r = "implement" -> { "reproduce" }
      [] r = "green"     -> { "implement", "falsify" }
      [] r = "ci_gate"   -> { "green" }
      [] r = "converge"  -> { "ci_gate" }
      [] r = "rehearsal" -> { "converge" }
      [] r = "dissent"   -> { "converge" }
      [] r = "release"   -> { "rehearsal", "dissent" }
      [] r = "confirm"   -> { "release" }

\* Artifact path each node writes. Convergence rounds carry the round index — that
\* suffix is exactly what keeps iterated writers disjoint. Drop it and
\* NoResourceCollision fails (its teeth). Fixed nodes write role-named paths.
ArtifactPath(r) == r
RoundPath(i)    == "converge-round-" \o ToString(i)

\* ==========================================================================
\* State
\* ==========================================================================
VARIABLES
    status,          \* [Roles -> {"Pending", "Done", "Blocked"}]  node drainage
    written,         \* SUBSET of artifact paths written (collision witness)
    \* per-gate machine verdicts — "absent" until the gate runs; a gate reads
    \* evidence that MAY be absent, and absence REFUSES (GateFailClosed).
    reproduce_v,     \* "absent" | "PASS" | "BLOCKED"
    falsify_v,       \* "absent" | "PASS" | "BLOCKED"
    green_v,         \* "absent" | "PASS" | "BLOCKED"
    ci_v,            \* "absent" | "PASS" | "BLOCKED"
    dissent_v,       \* "absent" | "PASS" | "BLOCKED"  (BLOCKED when brief empty)
    rehearsal_v,     \* "absent" | "PASS" | "BLOCKED"
    \* the §6bis convergence loop
    round,           \* 0..MaxRounds  — the current round counter
    claude_leg,      \* "absent" | "clean" | "findings"  (review-claude verdict)
    codex_leg,       \* "absent" | "clean" | "findings"  (review-codex-sol verdict)
    converge_v,      \* "NONE" | "CLEAN" | "BLOCKED"     (the loop's folded verdict)
    written_rounds,  \* SUBSET of RoundPath(i) already written (round collision)
    release_v        \* "NONE" | "SHIP" | "REWRITE"      (the release decision)

vars == << status, written, reproduce_v, falsify_v, green_v, ci_v, dissent_v,
           rehearsal_v, round, claude_leg, codex_leg, converge_v,
           written_rounds, release_v >>

\* A node is runnable when it is still Pending and every dependency is Done.
Runnable(r) ==
    /\ status[r] = "Pending"
    /\ \A d \in Deps(r) : status[d] = "Done"

\* --------------------------------------------------------------------------
\* Fail-closed gate decisions (GateFailClosed lives here).
\* --------------------------------------------------------------------------
\* A generic single-evidence gate: PASS iff the evidence is present-and-passing;
\* absence or failure REFUSES.
GateDecision(ev) == IF ev = "pass" THEN "PASS" ELSE "BLOCKED"

\* The convergence fold: CLEAN iff BOTH engines are clean in the current round;
\* absence of either engine's verdict is NOT clean (fail-closed).
ConvergeDecision(cl, cx) ==
    IF cl = "clean" /\ cx = "clean" THEN "CLEAN" ELSE "NOT-CLEAN"

\* The release decision — SHIP only when EVERY upstream gate promoted: reproduce,
\* falsify, green, ci PASS; the convergence is CLEAN; rehearsal PASS; and the
\* dissent field is non-empty (dissent_v = "PASS"). Any absence => REWRITE.
ReleaseDecision ==
    IF /\ reproduce_v = "PASS"
       /\ falsify_v   = "PASS"
       /\ green_v     = "PASS"
       /\ ci_v        = "PASS"
       /\ converge_v  = "CLEAN"
       /\ rehearsal_v = "PASS"
       /\ dissent_v   = "PASS"
       THEN "SHIP" ELSE "REWRITE"

\* ==========================================================================
\* Init
\* ==========================================================================
Init ==
    /\ status         = [r \in Roles |-> "Pending"]
    /\ written        = {}
    /\ reproduce_v    = "absent"
    /\ falsify_v      = "absent"
    /\ green_v        = "absent"
    /\ ci_v           = "absent"
    /\ dissent_v      = "absent"
    /\ rehearsal_v    = "absent"
    /\ round          = 0
    /\ claude_leg     = "absent"
    /\ codex_leg      = "absent"
    /\ converge_v     = "NONE"
    /\ written_rounds = {}
    /\ release_v      = "NONE"

\* ==========================================================================
\* Actions
\* ==========================================================================

\* Generic drainage for the plain nodes (no special verdict effect): trace,
\* intake, contract, implement. They go Done and record their write.
PlainRoles == { "trace", "intake", "contract", "implement" }

ExecutePlain(r) ==
    /\ r \in PlainRoles
    /\ Runnable(r)
    /\ status'  = [status EXCEPT ![r] = "Done"]
    /\ written' = written \cup { ArtifactPath(r) }
    /\ UNCHANGED << reproduce_v, falsify_v, green_v, ci_v, dissent_v, rehearsal_v,
                    round, claude_leg, codex_leg, converge_v, written_rounds,
                    release_v >>

\* reproduce / falsify / green / ci / rehearsal — each emits a verdict that MAY be
\* absent (a completed node that produced no verdict.json is the silent-degrade the
\* gate must survive), so TLC explores {pass, blocked, absent}. The gate folds it
\* fail-closed via GateDecision (absent => BLOCKED). Each action is inlined (TLC
\* does not take a higher-order setter).
ExecuteReproduce ==
    /\ Runnable("reproduce")
    /\ status'      = [status EXCEPT !["reproduce"] = "Done"]
    /\ written'     = written \cup { ArtifactPath("reproduce") }
    /\ \E ev \in {"pass", "blocked", "absent"} : reproduce_v' = GateDecision(ev)
    /\ UNCHANGED << falsify_v, green_v, ci_v, dissent_v, rehearsal_v, round,
                    claude_leg, codex_leg, converge_v, written_rounds, release_v >>

ExecuteFalsify ==
    /\ Runnable("falsify")
    /\ status'      = [status EXCEPT !["falsify"] = "Done"]
    /\ written'     = written \cup { ArtifactPath("falsify") }
    /\ \E ev \in {"pass", "blocked", "absent"} : falsify_v' = GateDecision(ev)
    /\ UNCHANGED << reproduce_v, green_v, ci_v, dissent_v, rehearsal_v, round,
                    claude_leg, codex_leg, converge_v, written_rounds, release_v >>

ExecuteGreen ==
    /\ Runnable("green")
    /\ status'      = [status EXCEPT !["green"] = "Done"]
    /\ written'     = written \cup { ArtifactPath("green") }
    /\ \E ev \in {"pass", "blocked", "absent"} : green_v' = GateDecision(ev)
    /\ UNCHANGED << reproduce_v, falsify_v, ci_v, dissent_v, rehearsal_v, round,
                    claude_leg, codex_leg, converge_v, written_rounds, release_v >>

ExecuteCi ==
    /\ Runnable("ci_gate")
    /\ status'      = [status EXCEPT !["ci_gate"] = "Done"]
    /\ written'     = written \cup { ArtifactPath("ci_gate") }
    /\ \E ev \in {"pass", "blocked", "absent"} : ci_v' = GateDecision(ev)
    /\ UNCHANGED << reproduce_v, falsify_v, green_v, dissent_v, rehearsal_v, round,
                    claude_leg, codex_leg, converge_v, written_rounds, release_v >>

ExecuteRehearsal ==
    /\ Runnable("rehearsal")
    /\ status'      = [status EXCEPT !["rehearsal"] = "Done"]
    /\ written'     = written \cup { ArtifactPath("rehearsal") }
    /\ \E ev \in {"pass", "blocked", "absent"} : rehearsal_v' = GateDecision(ev)
    /\ UNCHANGED << reproduce_v, falsify_v, green_v, ci_v, dissent_v, round,
                    claude_leg, codex_leg, converge_v, written_rounds, release_v >>

\* dissent — the release-gate FIELD. An EMPTY brief is a hard-block (fail-closed):
\* TLC explores present-non-empty (PASS) and empty (BLOCKED).
ExecuteDissent ==
    /\ Runnable("dissent")
    /\ status'  = [status EXCEPT !["dissent"] = "Done"]
    /\ written' = written \cup { ArtifactPath("dissent") }
    /\ \E brief \in {"nonempty", "empty"} :
          dissent_v' = IF brief = "nonempty" THEN "PASS" ELSE "BLOCKED"
    /\ UNCHANGED << reproduce_v, falsify_v, green_v, ci_v, rehearsal_v, round,
                    claude_leg, codex_leg, converge_v, written_rounds, release_v >>

\* --------------------------------------------------------------------------
\* The §6bis convergence loop — a bounded round machine. `converge` is Runnable
\* when ci_gate is Done; it then iterates rounds 1..MaxRounds. Each round both
\* engines emit a verdict that MAY be absent; the fold is CLEAN only if both are
\* clean. On CLEAN it goes Done with converge_v = "CLEAN". At MaxRounds without a
\* clean fixpoint it goes Blocked with converge_v = "BLOCKED" (escalation) — NEVER
\* a silent pass.
\* --------------------------------------------------------------------------

\* Start / advance a review round (bounded by MaxRounds). Each round chooses the
\* two engine verdicts non-deterministically (absent models the silent-degrade).
ConvergeRound ==
    /\ status["converge"] = "Pending"
    /\ status["ci_gate"] = "Done"
    /\ round < MaxRounds
    /\ converge_v = "NONE"
    /\ round' = round + 1
    /\ \E cl \in {"clean", "findings", "absent"},
          cx \in {"clean", "findings", "absent"} :
          /\ claude_leg' = cl
          /\ codex_leg'  = cx
    /\ written_rounds' = written_rounds \cup { RoundPath(round + 1) }
    /\ UNCHANGED << status, written, reproduce_v, falsify_v, green_v, ci_v,
                    dissent_v, rehearsal_v, converge_v, release_v >>

\* The clean fixpoint — both engines clean in the current round: converge Done.
ConvergeClean ==
    /\ status["converge"] = "Pending"
    /\ round >= 1
    /\ ConvergeDecision(claude_leg, codex_leg) = "CLEAN"
    /\ status'    = [status EXCEPT !["converge"] = "Done"]
    /\ written'   = written \cup { ArtifactPath("converge") }
    /\ converge_v' = "CLEAN"
    /\ UNCHANGED << reproduce_v, falsify_v, green_v, ci_v, dissent_v, rehearsal_v,
                    round, claude_leg, codex_leg, written_rounds, release_v >>

\* Exhaustion — MaxRounds reached without a clean fixpoint: converge BLOCKED
\* (typed escalation, NOT a silent pass; NOT-CONVERGED blocks like NOT-RUN).
ConvergeExhausted ==
    /\ status["converge"] = "Pending"
    /\ round = MaxRounds
    /\ ConvergeDecision(claude_leg, codex_leg) # "CLEAN"
    /\ status'    = [status EXCEPT !["converge"] = "Blocked"]
    /\ converge_v' = "BLOCKED"
    /\ UNCHANGED << written, reproduce_v, falsify_v, green_v, ci_v, dissent_v,
                    rehearsal_v, round, claude_leg, codex_leg, written_rounds,
                    release_v >>

\* release — the fail-closed SHIP/REWRITE gate over the WHOLE dossier. Runnable
\* when rehearsal + dissent are Done (the diamond reconverges). It never SHIPs
\* unless every upstream gate promoted (ReleaseDecision).
ExecuteRelease ==
    /\ Runnable("release")
    /\ status'   = [status EXCEPT !["release"] = "Done"]
    /\ written'  = written \cup { ArtifactPath("release") }
    /\ release_v' = ReleaseDecision
    /\ UNCHANGED << reproduce_v, falsify_v, green_v, ci_v, dissent_v, rehearsal_v,
                    round, claude_leg, codex_leg, converge_v, written_rounds >>

\* confirm — the external replay + closure. Plain drainage once release is Done.
ExecuteConfirm ==
    /\ Runnable("confirm")
    /\ status'  = [status EXCEPT !["confirm"] = "Done"]
    /\ written' = written \cup { ArtifactPath("confirm") }
    /\ UNCHANGED << reproduce_v, falsify_v, green_v, ci_v, dissent_v, rehearsal_v,
                    round, claude_leg, codex_leg, converge_v, written_rounds,
                    release_v >>

\* Blocked cascade — a Pending node with a Blocked dependency can never become
\* Runnable, so it inherits the block (the mission escalates as a whole; the human
\* takes over the blocked branch). This is what makes the DAG TERMINATE when the
\* convergence loop escalates at MaxRounds: downstream nodes reach the terminal
\* Blocked state instead of spinning Pending forever. It models cosmon's real
\* behaviour — a blocked upstream strands its dependents until a human intervenes.
ExecuteBlockedCascade(r) ==
    /\ status[r] = "Pending"
    /\ \E d \in Deps(r) : status[d] = "Blocked"
    /\ status' = [status EXCEPT ![r] = "Blocked"]
    /\ UNCHANGED << written, reproduce_v, falsify_v, green_v, ci_v, dissent_v,
                    rehearsal_v, round, claude_leg, codex_leg, converge_v,
                    written_rounds, release_v >>

Next ==
    \/ \E r \in PlainRoles : ExecutePlain(r)
    \/ ExecuteReproduce
    \/ ExecuteFalsify
    \/ ExecuteGreen
    \/ ExecuteCi
    \/ ExecuteRehearsal
    \/ ExecuteDissent
    \/ ConvergeRound
    \/ ConvergeClean
    \/ ConvergeExhausted
    \/ ExecuteRelease
    \/ ExecuteConfirm
    \/ \E r \in Roles : ExecuteBlockedCascade(r)

\* Weak fairness so drainage / convergence is guaranteed (liveness). converge
\* reaches a terminal (Done or Blocked) because the round counter is bounded.
Fairness ==
    /\ \A r \in PlainRoles : WF_vars(ExecutePlain(r))
    /\ WF_vars(ExecuteReproduce)
    /\ WF_vars(ExecuteFalsify)
    /\ WF_vars(ExecuteGreen)
    /\ WF_vars(ExecuteCi)
    /\ WF_vars(ExecuteRehearsal)
    /\ WF_vars(ExecuteDissent)
    /\ WF_vars(ConvergeRound)
    /\ WF_vars(ConvergeClean)
    /\ WF_vars(ConvergeExhausted)
    /\ WF_vars(ExecuteRelease)
    /\ WF_vars(ExecuteConfirm)
    /\ \A r \in Roles : WF_vars(ExecuteBlockedCascade(r))

Spec == Init /\ [][Next]_vars /\ Fairness

\* ==========================================================================
\* TypeOK
\* ==========================================================================
TypeOK ==
    /\ status         \in [Roles -> {"Pending", "Done", "Blocked"}]
    /\ written         \subseteq { ArtifactPath(r) : r \in Roles }
    /\ reproduce_v    \in {"absent", "PASS", "BLOCKED"}
    /\ falsify_v      \in {"absent", "PASS", "BLOCKED"}
    /\ green_v        \in {"absent", "PASS", "BLOCKED"}
    /\ ci_v           \in {"absent", "PASS", "BLOCKED"}
    /\ dissent_v      \in {"absent", "PASS", "BLOCKED"}
    /\ rehearsal_v    \in {"absent", "PASS", "BLOCKED"}
    /\ round          \in 0..MaxRounds
    /\ claude_leg     \in {"absent", "clean", "findings"}
    /\ codex_leg      \in {"absent", "clean", "findings"}
    /\ converge_v     \in {"NONE", "CLEAN", "BLOCKED"}
    /\ written_rounds \subseteq { RoundPath(i) : i \in 1..MaxRounds }
    /\ release_v      \in {"NONE", "SHIP", "REWRITE"}

\* ==========================================================================
\* Property 1 — Termination (liveness): every node reaches a TERMINAL state
\* (Done, or Blocked for the converge escalation). Acyclic DAG + a convergence
\* loop bounded by MaxRounds + weak fairness => no cycle, no unbounded foaming.
\* ==========================================================================
Terminal(r) == status[r] \in {"Done", "Blocked"}
Termination == \A r \in Roles : <>Terminal(r)

\* ==========================================================================
\* Property 2 — GateFailClosed (LOAD-BEARING, safety). No gate promotes on absent
\* or failing evidence.
\* ==========================================================================
\* (a) a gate verdict is PASS only if it was not absent (absence refuses).
GateNeverPromotesAbsent ==
    /\ (reproduce_v = "PASS") => (status["reproduce"] = "Done")
    /\ (green_v     = "PASS") => (status["green"]     = "Done")
    /\ (ci_v        = "PASS") => (status["ci_gate"]   = "Done")

\* (b) the convergence is CLEAN only if BOTH engines were clean in the final round.
ConvergeCleanImpliesBothClean ==
    (converge_v = "CLEAN") => (claude_leg = "clean" /\ codex_leg = "clean")

\* (c) an absent engine verdict can never yield a CLEAN convergence (fail-closed).
AbsentEngineNeverClean ==
    (claude_leg = "absent" \/ codex_leg = "absent") => converge_v # "CLEAN"

\* (d) exhaustion is a typed BLOCKED escalation, never a silent CLEAN.
ExhaustionBlocks ==
    (status["converge"] = "Blocked") => converge_v = "BLOCKED"

\* (e) release SHIPs only when EVERY upstream gate promoted AND the dissent field
\* is non-empty. This is the whole-manifest validation (blueprint §8): release
\* never infers success from completion.
ShipImpliesAllGatesPromoted ==
    (release_v = "SHIP") =>
        /\ reproduce_v = "PASS"
        /\ falsify_v   = "PASS"
        /\ green_v     = "PASS"
        /\ ci_v        = "PASS"
        /\ converge_v  = "CLEAN"
        /\ rehearsal_v = "PASS"
        /\ dissent_v   = "PASS"

GateFailClosed ==
    /\ GateNeverPromotesAbsent
    /\ ConvergeCleanImpliesBothClean
    /\ AbsentEngineNeverClean
    /\ ExhaustionBlocks
    /\ ShipImpliesAllGatesPromoted

\* ==========================================================================
\* Property 3 — NoResourceCollision (safety): no two distinct Done nodes write the
\* same artifact path, and no two convergence rounds write the same round path
\* (the round index is what keeps iterated writers disjoint).
\* ==========================================================================
NoResourceCollision ==
    /\ \A m, n \in Roles :
          (m # n /\ status[m] = "Done" /\ status[n] = "Done")
              => ArtifactPath(m) # ArtifactPath(n)
    /\ \A i, j \in 1..MaxRounds :
          (i # j) => RoundPath(i) # RoundPath(j)

\* ==========================================================================
\* Property 4 — DeterministicParametrization (safety): the node set is a pure
\* function of the params. risk / review_scale / max_rounds are POSTURE params
\* that never alter the node set — so the set equals its param-independent image
\* in every state, and the convergence fan-out is bounded by MaxRounds.
\* ==========================================================================
ExpandedRoles ==
    { "trace", "intake", "contract", "reproduce", "falsify", "implement",
      "green", "ci_gate", "converge", "rehearsal", "dissent", "release",
      "confirm" }

DeterministicParametrization ==
    /\ Roles = ExpandedRoles
    /\ Cardinality(Roles) = 13
    /\ round <= MaxRounds

\* ==========================================================================
\* Bundled invariant (the safety set; Termination is a temporal PROPERTY).
\* ==========================================================================
SealInvariant ==
    /\ TypeOK
    /\ GateFailClosed
    /\ NoResourceCollision
    /\ DeterministicParametrization

=============================================================================
