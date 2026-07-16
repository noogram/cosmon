-------------------------- MODULE CosmonRunShare --------------------------
\* Cross-galaxy sharing discipline over the cosmon run plane.
\*
\* Gödel fragment — structural reproduction of delib-20260419-fe35
\* responses/godel.md (task-20260419-e2b6). The ≈50-line core is the
\* Gödel-persona sketch; the surrounding prose is this module's
\* editorial framing.
\*
\* EXTENDS:
\*   • CosmonRun          (I1..I10 — ADR-052 ground: fleet / molecule /
\*                         event-log state machine and out-of-band I9)
\*   • CosmonRunXGalaxy   (I11..I15 — delib-20260419-29f9 substrate:
\*                         cross-galaxy edges, galaxy identity, inter-
\*                         galaxy visibility)
\*
\* This module adds four operator actions over cross-galaxy artefacts —
\* Share, Detect, Collapse, Redact — and four invariants I16..I19,
\* culminating in the ShareHonesty Gödel sentence (I19), an out-of-band
\* property by construction of the same shape as I9 in CosmonRun.
\*
\* ═══════════════════ LOAD-BEARING DESIGN NOTE ═══════════════════
\* Share(a) MUST be preceded by Detect(a, p) for every perimeter p
\* ATOMICALLY, in the same `cs` invocation. A naive split — e.g. the
\* operator runs `cs detect a` then later `cs share a` — admits
\* interleaving with concurrent writers: `confidential[a]` may flip,
\* a new perimeter may appear, a `leak[a]` may materialise between
\* the two calls, and the I16 guarantee evaporates. The `cs share`
\* command therefore sequences Detect internally and refuses to
\* commit until every perimeter is observed in the same critical
\* section. At the specification level this appears as: Share(a)
\* carries `\A p \in Perimeter : detected[<<a,p>>]` as a guard
\* evaluated in the SAME STEP that writes `shared`. Any
\* implementation that relaxes this atomicity falsifies I16 and, by
\* the contrapositive route through I17, silently strips the
\* override discipline.
\* ═══════════════════════════════════════════════════════════════

EXTENDS CosmonRun, CosmonRunXGalaxy

CONSTANT Perimeter

VARIABLES
    confidential,     \* Mol -> BOOLEAN           ; marked confidential
    detected,         \* (Mol \X Perimeter) -> BOOLEAN ; Detect result
    shared,           \* Mol -> SUBSET Perimeter  ; perimeters reached
    override_used,    \* Mol -> BOOLEAN           ; explicit operator override
    leak              \* Mol -> BOOLEAN           ; known leak flag

share_vars == <<confidential, detected, shared, override_used, leak>>

ShareInit ==
    /\ confidential  \in [Mol -> BOOLEAN]
    /\ detected      = [mp \in Mol \X Perimeter |-> FALSE]
    /\ shared        = [m \in Mol |-> {}]
    /\ override_used = [m \in Mol |-> FALSE]
    /\ leak          = [m \in Mol |-> FALSE]

\* ---------------- Operator actions ----------------

Detect(a, p) ==
    /\ detected' = [detected EXCEPT ![<<a,p>>] = TRUE]
    /\ UNCHANGED <<confidential, shared, override_used, leak>>

\* Share(a) is atomic: precondition reads detected[a,p] for every p and
\* commits `shared` in the same step.
Share(a) ==
    /\ \A p \in Perimeter : detected[<<a,p>>]
    /\ (confidential[a] => override_used[a])
    /\ shared' = [shared EXCEPT ![a] = Perimeter]
    /\ UNCHANGED <<confidential, detected, override_used, leak>>

Collapse(a) ==
    /\ leak[a]
    /\ shared' = [shared EXCEPT ![a] = {}]
    /\ leak'   = [leak   EXCEPT ![a] = FALSE]
    /\ UNCHANGED <<confidential, detected, override_used>>

Redact(a, p) ==
    /\ p \in shared[a]
    /\ shared' = [shared EXCEPT ![a] = @ \ {p}]
    /\ leak'   = [leak   EXCEPT ![a] = FALSE]
    /\ UNCHANGED <<confidential, detected, override_used>>

ShareNext == \E a \in Mol, p \in Perimeter :
    Detect(a,p) \/ Share(a) \/ Collapse(a) \/ Redact(a,p)

ShareSpec ==
    /\ ShareInit
    /\ [][ShareNext]_share_vars
    /\ \A a \in Mol : WF_share_vars(Collapse(a))
    /\ \A a \in Mol, p \in Perimeter : WF_share_vars(Redact(a, p))

\* ---------------- Invariants (I16..I19) ----------------

\* I16 — safety. A confidential artefact is shared only under an
\* explicit operator override. The positive formulation.
I16_ConfidentialNeverShared == \A a \in Mol :
    (confidential[a] /\ shared[a] # {}) => override_used[a]

\* I17 — safety. The contrapositive as a separate theorem: override is
\* the ONLY escape. Without override, confidentiality implies empty
\* sharing. Redundant with I16 in the closed world, load-bearing as
\* documentation under extension.
I17_OverrideIsOnlyEscape == \A a \in Mol :
    (confidential[a] /\ ~override_used[a]) => shared[a] = {}

\* I18 — liveness. Every leak is eventually resolved, by Collapse or
\* Redact. WF on both actions makes this non-vacuous.
I18_LeakEventuallyResolved == \A a \in Mol : leak[a] ~> ~leak[a]

\* I19 — ShareHonesty. The Gödel sentence. Every non-empty `shared`
\* was preceded by a successful Detect at every perimeter. This holds
\* in-band (by Share's precondition) and FAILS when an out-of-band
\* writer mutates `shared` directly — exactly the shape of ADR-052's
\* I9. I19 is therefore an invariant of the CLOSED environment, not
\* of the CLI alone; naming it marks the honest frontier.
I19_ShareHonesty == \A a \in Mol :
    shared[a] # {} => (\A p \in Perimeter : detected[<<a,p>>])

=====================================================================
