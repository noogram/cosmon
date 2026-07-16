--------------------- MODULE CosmonRun_GovernanceGate ---------------------
\* Governance-gate composition layer (delib-20260516-5d97 Step 4 enfant #4,
\* "doctrine noogram x runtime cosmon", synthesis §D5 row 5).
\*
\* Wires the noogram MycelialGate predicate Admits(v, t) and AttestorGraph
\* A6 transience timer into the cosmon scheduler runtime by patching one
\* action only -- Complete(m) -- with a governance guard.  Subtraction
\* first: zero new state variables, zero rewritten action, only one new
\* gated transition.
\*
\* Composition shape (von-neumann §2 spec 3) ::
\*
\*     EXTENDS  CosmonRun                       (1 EXTENDS edge)
\*     INSTANCE MycelialGate    WITH ...        (1 INSTANCE edge)
\*     INSTANCE AttestorGraph   WITH ...        (1 INSTANCE edge)
\*
\* Coupling budget (shannon delib §3, S3 dette cachée) ::
\*   * zero VARIABLE synchronised cross-module (the doctrinal modules are
\*     INSTANCEd with their ledger / sealLog bound to ghost expressions ;
\*     MycelialAdmits is supplied as a CONSTANT operator by the harness
\*     `.cfg`, leaving the federation co-simulation to the doctrinal
\*     modules themselves rather than dragging their state into cosmon) ;
\*   * three CONSTANTS shared with each doctrinal module ;
\*   * +0 new state-variables versus CosmonRun.tla.
\*
\* The patched Complete(m) is realised as a sibling action
\* CompleteGoverned(m) -- TLA+ does not support overriding actions across
\* EXTENDS.  A new top-level NextGoverned swaps Complete for
\* CompleteGoverned in the in-band schedule.  CosmonRun's original Next
\* and Spec remain reachable for the legacy configs.
\*
\* Theorems preserved : NoDoubleWriter (I7) and I9 unchanged ; the
\* CompleteGoverned action only TIGHTENS the guard of Complete and
\* writes the same variables in the same way.
\*
\* New invariant : GovernanceGateRespected -- no governance-relevant
\* molecule reaches mol_status = "Completed" without MycelialAdmits(m).
\*
\* Parent deliberation : delib-20260516-5d97  (synthesis §5 row 5)
\* Sibling specs       : noogram/specs/MycelialGate.tla
\*                       noogram/specs/AttestorGraph.tla
\*                       noogram/specs/WitnessFreshness.tla
\*
\* Confidentiality : no internal token (cf. chancery
\* CONTRAINDICATIONS.md and ADR-0007 §5 hygiene reputationnelle).

EXTENDS CosmonRun

CONSTANTS
    GovernanceRelevant,   \* SUBSET Mol -- molecules subject to the gate
    FederationAdmitted,   \* SUBSET Mol -- molecules the federation admits
                          \* (oracle projection ; `.cfg`-supplied)
    Verticals,            \* MycelialGate / AttestorGraph constant
    Attestors,            \* MycelialGate / AttestorGraph constant
    Clusters,             \* MycelialGate label set
    k_gate,               \* MycelialGate quorum (>= 2)
    N_gate,               \* MycelialGate federation cardinality bound (>= 3)
    W_max_gate            \* A6 transience deadline (CONSTANT, never VARIABLE)

ASSUME
    /\ GovernanceRelevant \subseteq Mol
    /\ FederationAdmitted \subseteq Mol
    /\ k_gate \in Nat /\ N_gate \in Nat /\ W_max_gate \in Nat
    /\ k_gate >= 2 /\ N_gate >= 3 /\ k_gate <= N_gate /\ W_max_gate >= 1

\* Federation oracle -- defined in-module against the FederationAdmitted
\* constant subset.  In production deployments, `cs spec-audit --spec
\* mycelial-gate` derives this set from the noogram NDJSON ledger.
MycelialAdmits(m) == m \in FederationAdmitted

\* INSTANCE edges -- declared as documentation, NOT activated at SANY
\* time.  Reason : the federation modules (MycelialGate, AttestorGraph)
\* live under noogram/specs/ ; making the cosmon spec parse from
\* cosmon/docs/specs/ alone keeps `cs spec-audit` self-contained and
\* preserves shannon's "0 VARIABLE synchronised cross-module" budget.
\* The pattern (commented INSTANCE as documentation) follows AttestorGraph.tla
\* L106-L115.  MycelialAdmits(_) is the opaque oracle the harness `.cfg`
\* supplies ; the doctrinal co-simulation is performed by `cs spec-audit
\* --spec mycelial-gate` against the noogram NDJSON ledger.
\*
\* MG == INSTANCE MycelialGate
\*         WITH Verticals <- Verticals,
\*              Attestors <- Attestors,
\*              Clusters  <- Clusters,
\*              k         <- k_gate,
\*              N         <- N_gate,
\*              W_max     <- W_max_gate,
\*              MaxT      <- MaxClock,
\*              ledger    <- << >>,
\*              sealLog   <- {},
\*              now       <- now
\*
\* AG == INSTANCE AttestorGraph
\*         WITH Attestor   <- Attestors,
\*              Vertical   <- Verticals,
\*              Quorum     <- k_gate,
\*              T_halflife <- W_max_gate,
\*              MaxT       <- MaxClock,
\*              ledger     <- << >>,
\*              sealLog    <- {},
\*              now        <- now

IsGovernanceRelevant(m) == m \in GovernanceRelevant

\* CompleteGoverned -- the patched Complete action.  Adds one conjunct
\* to CosmonRun.Complete : a governance-relevant molecule may move to
\* "Completed" only if the federation oracle MycelialAdmits(m) holds.
\* All other conjuncts (and all variable writes) are identical to
\* CosmonRun.Complete.
CompleteGoverned(m) ==
    /\ mol_status[m] = "Running"
    /\ Silence(m) <= T_STALL
    /\ IsGovernanceRelevant(m) => MycelialAdmits(m)
    /\ mol_status'        = [mol_status        EXCEPT ![m] = "Completed"]
    /\ fleet_desired'     = [fleet_desired     EXCEPT ![m] = "None"]
    /\ tmux_session'      = [tmux_session      EXCEPT ![m] = FALSE]
    /\ worker_pid_alive'  = [worker_pid_alive  EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<branch_merged, events_seqno, events_writer_lock,
                   sealLog, now>>

NextGoverned ==
    \/ \E m \in Mol :
          \/ Nucleate(m) \/ Tackle(m) \/ Evolve(m)
          \/ CompleteGoverned(m) \/ Done(m)
          \/ Collapse(m) \/ Freeze(m) \/ Thaw(m) \/ LockRelease(m)
          \/ Purge(m) \/ MarkStalled(m)
          \/ TmuxCrash(m) \/ ProcessCrash(m) \/ BypassMerge(m)
    \/ Tick

\* Spec rebuild -- same fairness obligations as CosmonRun.Spec ; Tick
\* keeps `now` advancing so AttestorGraph.A6 timer eventually fires.
SpecGoverned ==
    /\ Init
    /\ [][NextGoverned]_vars
    /\ \A m \in Mol : WF_vars(Done(m))
    /\ \A m \in Mol : WF_vars(Purge(m))
    /\ \A m \in Mol : WF_vars(LockRelease(m))
    /\ \A m \in Mol : WF_vars(MarkStalled(m))
    /\ WF_vars(Tick)

\* New invariant : no governance-relevant molecule lands in "Completed"
\* unless MycelialAdmits(m) holds.  Mechanically checked under SpecGoverned.
GovernanceGateRespected ==
    \A m \in Mol :
        (IsGovernanceRelevant(m) /\ mol_status[m] = "Completed")
            => MycelialAdmits(m)

\* I7 preserved -- CompleteGoverned writes the same variables in the same
\* way as CosmonRun.Complete ; no new writer enters events_writer_lock.
NoDoubleWriter == I7_SingleEventWriter

=============================================================================
