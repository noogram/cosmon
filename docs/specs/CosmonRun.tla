-------------------------- MODULE CosmonRun --------------------------
\* Mechanical formalisation of the cosmon "ten invariants" enumerated
\* in ADR-052 ("One Ledger, One Writer, One Witness per Field").
\*
\* The spec skeleton was produced by the deliberation
\* delib-20260419-d34b synthesis.md §(c) (Gödel persona, ~80 LOC).
\* This file extends it on three points the synthesis omits but which
\* the task brief requires (task-20260419-af4c):
\*
\*   1. Purge(m)        — drops a stale fleet entry when its worker
\*                        process is no longer alive. Without it,
\*                        liveness L2 has no enabled action.
\*
\*   2. AsyncCrashesEnabled — CONSTANT gate that enables / disables
\*                        the asynchronous environment actions
\*                        TmuxCrash and ProcessCrash. With them OFF
\*                        the spec runs "in-band" (everything happens
\*                        through the cosmon CLI). With them ON the
\*                        spec includes the out-of-band ground-truth
\*                        slips that the watchdog must reconcile.
\*
\*   3. OutOfBandEnabled  — CONSTANT gate that enables / disables
\*                        BypassMerge(m), an adversarial action that
\*                        marks a branch merged WITHOUT going through
\*                        Done. This is the c1cb-style external git
\*                        merge of 2026-04-19. Used to exhibit I9 as
\*                        the Gödel sentence (cf. ADR-052 §Out-of-band).
\*
\* CONSTANT MaxSeqno bounds events_seqno so the state space is finite.
\*
\* Extended 2026-04-20 (task-20260420-ea09) with I_StepProgress — the
\* eighth-clock (hawking, `StepClock`) liveness invariant that the
\* 1b02 panel predicted before it was written. It formalises the
\* inference-stall pathology (the fixture `idea-20260419-2d4e`):
\*
\*   * Variables `now` and `sealLog` encode the step-progress clock.
\*     `sealLog[m]` is a bounded sequence of timestamps of emitted
\*     briefing seals (knuth §1). `Silence(m) = now - last_seal_t` is
\*     the MSS of the stall — the cosmological horizon between
\*     α-emission (worker turn) and γ-observation (cosmon filesystem).
\*
\*   * `MarkStalled(m)` is the cosmon-side (not worker-side) detector:
\*     Running ∧ Silence(m) > T_STALL ⇒ Stalled. It is the α-channel
\*     turing proved semi-decidable only (Rice-adjacent) — the TLA
\*     encoding treats it as a weak-fair observer action, not as a
\*     safety invariant.
\*
\*   * `Complete(m)` gains a guard `Silence(m) ≤ T_STALL`. A worker
\*     that has stopped emitting seals cannot plausibly issue a
\*     completion — shannon's "adversarial slow-step capacity = 0 bits".
\*     The guard is vacuous when `T_STALL ≥ MaxClock` (the default for
\*     the eight pre-existing configs), so the older models are
\*     unchanged.
\*
\*   * `I_StepProgress` is the leads-to:
\*       (Running ∧ Silence > T_STALL) ~> mol_status ∈ {Stalled,
\*                                                      Collapsed,
\*                                                      Frozen}
\*     under weak fairness on `MarkStalled` and `Tick`. The consequent
\*     is broadened beyond `Stalled` because operator-initiated
\*     Collapse/Freeze are legitimate resolutions of a detected stall
\*     (Feynman's 8-year-old: the silent messenger is noticed either
\*     by the kitchen clock or by the mother asking "are you asleep?").
\*
\*   * `GhostKind` collects the seven named drift shapes of the TLA
\*     model. The seventh, `InferenceStalled`, is the new one (turing,
\*     synthesis §C2). Rust-side GhostKind is extended separately by
\*     the polymer workstream; this spec is the formal proof obligation
\*     that justifies the enum variant.

EXTENDS Naturals, FiniteSets, Sequences, TLC

\* T_STALL  — silence threshold (clock ticks). MarkStalled becomes
\*            enabled once Silence(m) > T_STALL.
\* MaxClock — monotonic upper bound on `now` (state-space control).
\*            When T_STALL >= MaxClock the StepProgress machinery is
\*            effectively dormant — the default for the eight
\*            pre-existing configs.
CONSTANTS Mol, MaxSeqno, AsyncCrashesEnabled, OutOfBandEnabled,
          T_STALL, MaxClock

VARIABLES
    mol_status,              \* Mol -> {Pending,Running,Stalled,Completed,Collapsed,Frozen,Absent}
    fleet_desired,           \* Mol -> {None, Registered}
    tmux_session,            \* Mol -> BOOLEAN
    worker_pid_alive,        \* Mol -> BOOLEAN
    branch_merged,           \* Mol -> BOOLEAN
    events_seqno,            \* Mol -> 0..MaxSeqno
    events_writer_lock,      \* Mol -> {None, Worker}
    sealLog,                 \* Mol -> Seq(0..MaxClock) — briefing-seal timestamps
    now                      \* 0..MaxClock — monotonic global clock

vars == <<mol_status, fleet_desired, tmux_session, worker_pid_alive,
          branch_merged, events_seqno, events_writer_lock,
          sealLog, now>>

Init ==
    /\ mol_status         = [m \in Mol |-> "Absent"]
    /\ fleet_desired      = [m \in Mol |-> "None"]
    /\ tmux_session       = [m \in Mol |-> FALSE]
    /\ worker_pid_alive   = [m \in Mol |-> FALSE]
    /\ branch_merged      = [m \in Mol |-> FALSE]
    /\ events_seqno       = [m \in Mol |-> 0]
    /\ events_writer_lock = [m \in Mol |-> "None"]
    /\ sealLog            = [m \in Mol |-> <<>>]
    /\ now                = 0

\* ---------------- StepClock helpers (hawking 8th clock) ----------------

\* Timestamp of the most recent seal, or 0 when no seal has been emitted
\* yet. Knuth §1 — clock #4 (Witness clock) is the authoritative source
\* for silence duration; here we model the cosmon-side projection of
\* that clock (`events.jsonl` append times).
LastSealT(m) ==
    IF sealLog[m] = <<>>
    THEN 0
    ELSE sealLog[m][Len(sealLog[m])]

\* `Silence(m)` — how long since the worker last produced evidence of
\* progress. Monotone in `now`; reset to 0 by every Evolve. This is
\* the minimum sufficient statistic (shannon, C5) for classifying a
\* Running molecule as Stalled.
Silence(m) == now - LastSealT(m)

\* ---------------- In-band cosmon CLI actions ----------------

Nucleate(m) == /\ mol_status[m] = "Absent"
               /\ mol_status' = [mol_status EXCEPT ![m] = "Pending"]
               /\ UNCHANGED <<fleet_desired, tmux_session, worker_pid_alive,
                              branch_merged, events_seqno, events_writer_lock,
                              sealLog, now>>

Tackle(m) == /\ mol_status[m] = "Pending"
             /\ mol_status'    = [mol_status EXCEPT ![m] = "Running"]
             /\ fleet_desired' = [fleet_desired EXCEPT ![m] = "Registered"]
             /\ tmux_session'     = [tmux_session EXCEPT ![m] = TRUE]
             /\ worker_pid_alive' = [worker_pid_alive EXCEPT ![m] = TRUE]
             /\ UNCHANGED <<branch_merged, events_seqno, events_writer_lock,
                            sealLog, now>>

\* Evolve — advances one step AND emits a seal. The seal is the
\* α-emission hawking named as the missing 8th clock: a timestamped
\* entry in `sealLog[m]` that resets Silence(m) to 0.
Evolve(m) == /\ mol_status[m] = "Running"
             /\ worker_pid_alive[m] = TRUE
             /\ events_writer_lock[m] = "None"
             /\ events_seqno[m] < MaxSeqno
             /\ events_writer_lock' = [events_writer_lock EXCEPT ![m] = "Worker"]
             /\ events_seqno' = [events_seqno EXCEPT ![m] = @ + 1]
             /\ sealLog' = [sealLog EXCEPT ![m] = Append(@, now)]
             /\ UNCHANGED <<mol_status, fleet_desired, tmux_session,
                            worker_pid_alive, branch_merged, now>>

\* Complete — a worker can only declare completion while the step-
\* progress channel is live (Silence within threshold). A silent
\* worker (Silence > T_STALL) is by definition not producing turns
\* and therefore cannot plausibly emit `cs complete`. The guard is
\* vacuous in pre-2026-04-20 configs (T_STALL = 99 > MaxClock = 2).
Complete(m) == /\ mol_status[m] = "Running"
               /\ Silence(m) <= T_STALL
               /\ mol_status' = [mol_status EXCEPT ![m] = "Completed"]
               /\ fleet_desired' = [fleet_desired EXCEPT ![m] = "None"]
               /\ tmux_session'  = [tmux_session EXCEPT ![m] = FALSE]
               /\ worker_pid_alive' = [worker_pid_alive EXCEPT ![m] = FALSE]
               /\ UNCHANGED <<branch_merged, events_seqno, events_writer_lock,
                              sealLog, now>>

\* Done has TWO premises: status = Completed (in-band) AND not yet
\* merged (idempotence). It is the boss-stamps-ticket gesture.
Done(m) == /\ mol_status[m] = "Completed"
           /\ branch_merged[m] = FALSE
           /\ branch_merged' = [branch_merged EXCEPT ![m] = TRUE]
           /\ UNCHANGED <<mol_status, fleet_desired, tmux_session,
                          worker_pid_alive, events_seqno, events_writer_lock,
                          sealLog, now>>

\* Collapse accepts Stalled as a source state too — an operator can
\* always decide to bury a molecule the observer marked as stalled.
Collapse(m) == /\ mol_status[m] \in {"Pending","Running","Frozen","Stalled"}
               /\ mol_status' = [mol_status EXCEPT ![m] = "Collapsed"]
               /\ fleet_desired' = [fleet_desired EXCEPT ![m] = "None"]
               /\ tmux_session'  = [tmux_session EXCEPT ![m] = FALSE]
               /\ worker_pid_alive' = [worker_pid_alive EXCEPT ![m] = FALSE]
               /\ UNCHANGED <<branch_merged, events_seqno, events_writer_lock,
                              sealLog, now>>

Freeze(m) == /\ mol_status[m] = "Running"
             /\ mol_status' = [mol_status EXCEPT ![m] = "Frozen"]
             /\ UNCHANGED <<fleet_desired, tmux_session, worker_pid_alive,
                            branch_merged, events_seqno, events_writer_lock,
                            sealLog, now>>

Thaw(m) == /\ mol_status[m] = "Frozen"
           /\ mol_status' = [mol_status EXCEPT ![m] = "Running"]
           /\ UNCHANGED <<fleet_desired, tmux_session, worker_pid_alive,
                          branch_merged, events_seqno, events_writer_lock,
                          sealLog, now>>

LockRelease(m) == /\ events_writer_lock[m] # "None"
                  /\ events_writer_lock' = [events_writer_lock EXCEPT ![m] = "None"]
                  /\ UNCHANGED <<mol_status, fleet_desired, tmux_session,
                                 worker_pid_alive, branch_merged, events_seqno,
                                 sealLog, now>>

\* Purge — added beyond the synthesis skeleton to give L2 a target.
\* It is the patrol-watchdog action: detect dead worker, drop fleet entry.
Purge(m) == /\ fleet_desired[m] = "Registered"
            /\ ~worker_pid_alive[m]
            /\ fleet_desired' = [fleet_desired EXCEPT ![m] = "None"]
            /\ tmux_session'  = [tmux_session EXCEPT ![m] = FALSE]
            /\ UNCHANGED <<mol_status, worker_pid_alive,
                           branch_merged, events_seqno, events_writer_lock,
                           sealLog, now>>

\* ---------------- StepClock / InferenceStalled (the 7th ghost) ---------

\* MarkStalled — COSMON-SIDE observer action (ADR-052 I2: the worker
\* cannot self-certify Presence; shannon D1). The patrol — not the
\* worker — transitions a Running molecule to Stalled when Silence
\* exceeds the per-formula T_STALL heuristic. Turing's decidability
\* caveat: this is a semi-decidable classification; the TLA property
\* `I_StepProgress` is therefore liveness (leads-to), not safety.
MarkStalled(m) == /\ mol_status[m] = "Running"
                  /\ Silence(m) > T_STALL
                  /\ mol_status' = [mol_status EXCEPT ![m] = "Stalled"]
                  /\ UNCHANGED <<fleet_desired, tmux_session, worker_pid_alive,
                                 branch_merged, events_seqno, events_writer_lock,
                                 sealLog, now>>

\* Tick — monotone global clock. Bounded by MaxClock for TLC. Weak
\* fairness on Tick ensures `now` can actually grow, which is a
\* precondition for Silence ever exceeding T_STALL in a non-vacuous
\* way.
Tick == /\ now < MaxClock
        /\ now' = now + 1
        /\ UNCHANGED <<mol_status, fleet_desired, tmux_session,
                       worker_pid_alive, branch_merged, events_seqno,
                       events_writer_lock, sealLog>>

\* ---------------- Out-of-band ground-truth (asynchronous) ----------------

TmuxCrash(m) == /\ AsyncCrashesEnabled
                /\ tmux_session[m]
                /\ tmux_session' = [tmux_session EXCEPT ![m] = FALSE]
                /\ UNCHANGED <<mol_status, fleet_desired, worker_pid_alive,
                               branch_merged, events_seqno, events_writer_lock,
                               sealLog, now>>

ProcessCrash(m) == /\ AsyncCrashesEnabled
                   /\ worker_pid_alive[m]
                   /\ worker_pid_alive' = [worker_pid_alive EXCEPT ![m] = FALSE]
                   /\ UNCHANGED <<mol_status, fleet_desired, tmux_session,
                                  branch_merged, events_seqno, events_writer_lock,
                                  sealLog, now>>

\* BypassMerge — adversarial out-of-band action. Marks a branch merged
\* without consulting mol_status. Models the c1cb incident of
\* 2026-04-19 (manual `git merge` from another shell, bypassing
\* `cs done`). Gated by OutOfBandEnabled so the in-band model
\* disables it.
BypassMerge(m) == /\ OutOfBandEnabled
                  /\ branch_merged[m] = FALSE
                  /\ branch_merged' = [branch_merged EXCEPT ![m] = TRUE]
                  /\ UNCHANGED <<mol_status, fleet_desired, tmux_session,
                                 worker_pid_alive, events_seqno, events_writer_lock,
                                 sealLog, now>>

\* ---------------- Next-state and fairness ----------------

Next == \/ \E m \in Mol :
              Nucleate(m) \/ Tackle(m) \/ Evolve(m) \/ Complete(m) \/ Done(m)
              \/ Collapse(m) \/ Freeze(m) \/ Thaw(m) \/ LockRelease(m)
              \/ Purge(m) \/ MarkStalled(m)
              \/ TmuxCrash(m) \/ ProcessCrash(m) \/ BypassMerge(m)
        \/ Tick

\* Weak fairness on the actions whose absence would falsify the
\* corresponding liveness properties:
\*   * Done       — I5 (CompletedEventuallyMerges)
\*   * Purge      — L2 (DeadSessionEventuallyPurged)
\*   * LockRelease — L3 (LockEventuallyReleased)
\*   * MarkStalled — I_StepProgress (the new invariant)
\*   * Tick        — required so `now` can actually advance; without
\*                   it TLC can pick the "time stands still" trace
\*                   that trivially satisfies I_StepProgress.
Spec == /\ Init
        /\ [][Next]_vars
        /\ \A m \in Mol : WF_vars(Done(m))
        /\ \A m \in Mol : WF_vars(Purge(m))
        /\ \A m \in Mol : WF_vars(LockRelease(m))
        /\ \A m \in Mol : WF_vars(MarkStalled(m))
        /\ WF_vars(Tick)

\* ---------------- Safety invariants (ADR-052 I3..I7, I9) ----------------

\* I3 — fleet entry implies tmux session exists. Holds in-band only;
\* TmuxCrash temporarily violates it until Purge runs (this is the
\* eventual-consistency frontier the validation report documents).
I3_FleetMirrorsSession == \A m \in Mol :
    fleet_desired[m] = "Registered" => tmux_session[m]

\* I4 — tmux session implies live worker process. Same in-band caveat
\* as I3.
I4_SessionImpliesLiveProcess == \A m \in Mol :
    tmux_session[m] => worker_pid_alive[m]

\* I6 — fleet entry only for Running/Frozen/Stalled molecules. Note
\* that Complete atomically clears fleet_desired here (we strengthened
\* the synthesis Complete to do this), so I6 holds in-band as stepwise
\* safety. Stalled is included because MarkStalled does not touch
\* fleet_desired — the observer merely labels; teardown stays with
\* the operator (Collapse / Done).
I6_NoGhostFleetEntry == \A m \in Mol :
    fleet_desired[m] = "Registered" => mol_status[m] \in {"Running","Frozen","Stalled"}

\* I7 — at most one writer holds the events.jsonl lock. Structurally
\* enforced by events_writer_lock being a single-cell variable per
\* molecule whose codomain is {None, Worker}.
I7_SingleEventWriter == \A m \in Mol :
    events_writer_lock[m] \in {"None","Worker"}

\* I9 — branch may be merged ONLY for Completed (or Collapsed) molecules.
\* Holds in-band. FAILS when OutOfBandEnabled = TRUE — which is the
\* mechanical proof of ADR-052's "Out-of-band classification" of I9:
\* the property is true precisely when the environment is closed; it
\* cannot be enforced from inside the spec when the environment can
\* write branch_merged directly.
I9_BranchMergedOnlyIfCompleted == \A m \in Mol :
    branch_merged[m] => mol_status[m] \in {"Completed","Collapsed"}

\* ---------------- Liveness (I5 + I_StepProgress + supporting L2, L3) --

I5_CompletedEventuallyMerges == \A m \in Mol :
    (mol_status[m] = "Completed") ~> branch_merged[m]

L2_DeadSessionEventuallyPurged == \A m \in Mol :
    (fleet_desired[m] = "Registered" /\ ~worker_pid_alive[m])
        ~> (fleet_desired[m] = "None")

L3_LockEventuallyReleased == \A m \in Mol :
    (events_writer_lock[m] # "None") ~> (events_writer_lock[m] = "None")

\* I_StepProgress — knuth's eighth-clock leads-to (hawking's StepClock;
\* turing's InferenceStalled ghost). A Running molecule whose Silence
\* has crossed the per-formula threshold must eventually *resolve*:
\* either the worker comes back (Evolve refreshes the seal, dropping
\* Silence back under threshold) OR the system changes mol_status
\* out of Running (Stalled, Collapsed, Frozen, Completed). The honest
\* leads-to statement is therefore:
\*
\*     (Running ∧ Silence > T_STALL) ~>
\*         (Silence ≤ T_STALL  ∨  mol_status ≠ Running)
\*
\* This rules out the Sisyphus trace knuth §7 warned about — forever
\* Running, forever silent, never resolved. Under weak fairness on
\* MarkStalled and Tick the only escape routes are Evolve (worker
\* returns), MarkStalled (observer marks stalled), or an operator
\* action (Collapse/Freeze). `Complete` is ALSO a legitimate exit
\* because Complete is guarded by `Silence ≤ T_STALL` upstream — by
\* the time Complete fires, the "silent" episode has already ended.
I_StepProgress == \A m \in Mol :
    (mol_status[m] = "Running" /\ Silence(m) > T_STALL)
        ~> (Silence(m) <= T_STALL \/ mol_status[m] # "Running")

\* ---------------- TypeOK and state constraint ----------------

StatusValues == {"Absent","Pending","Running","Stalled",
                 "Completed","Collapsed","Frozen"}
FleetValues  == {"None","Registered"}
LockValues   == {"None","Worker"}

\* GhostKind — the seven named drift shapes of the cosmon TLA model.
\* Five of them (DeadPane, VanishedWorker, UnHarvested, StaleProbe,
\* UnnamedMerge) are the April 18–19 ghosts formalised in ADR-052's
\* Rust-side enum. `Sediment` is the scheduler-side drift named in
\* ADR-048 and proved by CosmonRunScheduler_ConvoyCascade. The new
\* seventh shape, `InferenceStalled`, corresponds to mol_status =
\* "Stalled" and is the proof obligation I_StepProgress discharges.
GhostKind == {"DeadPane","VanishedWorker","UnHarvested",
              "StaleProbe","UnnamedMerge","Sediment","InferenceStalled"}

TypeOK ==
    /\ mol_status         \in [Mol -> StatusValues]
    /\ fleet_desired      \in [Mol -> FleetValues]
    /\ tmux_session       \in [Mol -> BOOLEAN]
    /\ worker_pid_alive   \in [Mol -> BOOLEAN]
    /\ branch_merged      \in [Mol -> BOOLEAN]
    /\ events_seqno       \in [Mol -> 0..MaxSeqno]
    /\ events_writer_lock \in [Mol -> LockValues]
    /\ sealLog            \in [Mol -> Seq(0..MaxClock)]
    /\ now                \in 0..MaxClock
=====================================================================
