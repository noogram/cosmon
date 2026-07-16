----------------------- MODULE CosmonRunScheduler -----------------------
\* Mechanical formalisation of the cosmon horology: five launchd
\* patrols (ADR-050) + the future cs-autopilot-tick action (ADR-053,
\* scope-checked per ADR-048).
\*
\* Patrols modelled: nightly-drain, temp-review, backlog-sanity,
\* patrol-propel, purge-stale. Clock is a bounded Nat (`clock`);
\* each patrol has a cadence (ticks between successful fires) and a
\* cron window (abstracted as a SUBSET of 0..MaxTime).
\*
\* Safety invariants:
\*   S1_NonOverlap         — at most one patrol fires at a time
\*                           (scheduler holds a mutex `lock`).
\*   S2_WindowIsClosed     — Fire(p) only inside cron_window[p].
\*   S3_PurgeBeforeRespawn — patrol-propel never fires while
\*                           sediment > 0. The 2026-04-12 convoy
\*                           cascade (recorded in an internal
\*                           chronicle) is the counterexample when
\*                           S3Enabled = FALSE.
\*
\* Liveness:
\*   L1_FireWithinWindow   — an enabled patrol eventually fires
\*                           (WF_vars(Fire(p))).
\*   L2_EventualFinish     — a firing patrol eventually finishes
\*                           (WF_vars(Finish(p)) or Timeout(p)).
\*
\* Autopilot extension: when AutopilotEnabled = TRUE an extra guard
\* blocks Propel if sediment >= BacklogThreshold (ADR-048 backlog
\* sanity). The autopilot tick is not a separate action — it rides
\* the same Fire/Finish cycle; the guard is what "scope_check" means
\* mechanically.
\*
\* Two model configs:
\*   Normal        — S3Enabled = TRUE;  no violation expected.
\*   ConvoyCascade — S3Enabled = FALSE; S3 violated (trace = convoy).

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS MaxTime, MaxSediment, S3Enabled,
          AutopilotEnabled, BacklogThreshold

\* Domain literals — not parameterised. These are the five plists
\* that ADR-050 migrates from `~/Library/LaunchAgents/` into one
\* unified scheduler, plus the two load-bearing names used by the
\* S3 guard and the Purge semantics.
Propel == "patrol-propel"
Purge  == "purge-stale"
Patrols == {"nightly-drain", "temp-review", "backlog-sanity",
            Propel, Purge}

\* Cadence: ticks between successful fires. Abstracted from the
\* real cron strings — "nightly-drain" is a once-a-night patrol, so
\* the longest cadence; "backlog-sanity" and "purge-stale" tick
\* cheaply.
Cadence == [p \in Patrols |->
    CASE p = "nightly-drain"  -> 3
      [] p = "temp-review"    -> 2
      [] p = "backlog-sanity" -> 1
      [] p = Propel           -> 2
      [] p = Purge            -> 1]

\* WindowOpen: bounded abstraction of the cron window. All patrols
\* can fire at any tick in this skeleton; S2 still catches any
\* Fire(p) that lands outside the declared window. Narrower windows
\* are a per-config concern.
WindowOpen == [p \in Patrols |-> 0..MaxTime]

VARIABLES clock, patrol_firing, next_fire_at, last_completion,
          lock, sediment, cascade_detected

vars == <<clock, patrol_firing, next_fire_at, last_completion,
          lock, sediment, cascade_detected>>

Init ==
    /\ clock            = 0
    /\ patrol_firing    = [p \in Patrols |-> FALSE]
    /\ next_fire_at     = [p \in Patrols |-> 0]
    /\ last_completion  = [p \in Patrols |-> 0]
    /\ lock             = "None"
    /\ sediment         = 0
    /\ cascade_detected = FALSE

\* ---------- Clock and environment ----------

Tick == /\ clock < MaxTime
        /\ lock = "None"
        /\ clock' = clock + 1
        /\ UNCHANGED <<patrol_firing, next_fire_at, last_completion,
                       lock, sediment, cascade_detected>>

\* External activity: stale molecules / dead workers accrue between
\* ticks. `purge-stale` reaps them; `patrol-propel` fired on non-
\* empty sediment is the cascade.
ActivityAccrues == /\ sediment < MaxSediment
                   /\ sediment' = sediment + 1
                   /\ UNCHANGED <<clock, patrol_firing, next_fire_at,
                                  last_completion, lock, cascade_detected>>

\* ---------- Scheduler actions ----------

CanFire(p) ==
    /\ ~patrol_firing[p]
    /\ lock = "None"
    /\ clock \in WindowOpen[p]
    /\ clock >= next_fire_at[p]
    /\ (p = Propel /\ S3Enabled)         => sediment = 0
    /\ (p = Propel /\ AutopilotEnabled)  => sediment < BacklogThreshold

Fire(p) ==
    /\ CanFire(p)
    /\ patrol_firing'    = [patrol_firing EXCEPT ![p] = TRUE]
    /\ lock'             = p
    /\ cascade_detected' =
           IF p = Propel /\ sediment > 0 THEN TRUE ELSE cascade_detected
    /\ UNCHANGED <<clock, next_fire_at, last_completion, sediment>>

Finish(p) ==
    /\ patrol_firing[p]
    /\ lock = p
    /\ patrol_firing'   = [patrol_firing EXCEPT ![p] = FALSE]
    /\ lock'            = "None"
    /\ next_fire_at'    = [next_fire_at EXCEPT ![p] = clock + Cadence[p]]
    /\ last_completion' = [last_completion EXCEPT ![p] = clock]
    /\ sediment'        = IF p = Purge THEN 0 ELSE sediment
    /\ UNCHANGED <<clock, cascade_detected>>

\* Timeout: launchd kills a wedged patrol. Same release of the lock
\* as Finish, but no completion credit (last_completion unchanged).
Timeout(p) ==
    /\ patrol_firing[p]
    /\ lock = p
    /\ patrol_firing' = [patrol_firing EXCEPT ![p] = FALSE]
    /\ lock'          = "None"
    /\ next_fire_at'  = [next_fire_at EXCEPT ![p] = clock + Cadence[p]]
    /\ UNCHANGED <<clock, last_completion, sediment, cascade_detected>>

Next == \/ Tick
        \/ ActivityAccrues
        \/ \E p \in Patrols : Fire(p) \/ Finish(p) \/ Timeout(p)

Spec == /\ Init
        /\ [][Next]_vars
        /\ \A p \in Patrols : WF_vars(Fire(p))
        /\ \A p \in Patrols : WF_vars(Finish(p))

\* ---------- Safety invariants ----------

S1_NonOverlap == \A p, q \in Patrols :
    (patrol_firing[p] /\ patrol_firing[q]) => p = q

S2_WindowIsClosed == \A p \in Patrols :
    patrol_firing[p] => clock \in WindowOpen[p]

\* S3: patrol-propel only fires while sediment = 0. The cascade_detected
\* sentinel is set inside Fire if Propel is fired on non-empty sediment;
\* checking ~cascade_detected is equivalent to: the cascade never happens.
S3_PurgeBeforeRespawn == ~cascade_detected

\* ---------- Liveness ----------

\* A firing patrol eventually releases the lock (Finish or Timeout).
L2_EventualFinish == \A p \in Patrols :
    patrol_firing[p] ~> ~patrol_firing[p]

\* A patrol whose cadence fires inside the window eventually fires,
\* provided the S3 / autopilot guards do not block it forever. Under
\* the Normal config (bounded sediment + WF on Fire) this holds;
\* under ConvoyCascade it is not checked.
L1_FireWithinWindow == \A p \in Patrols :
    (p # Propel /\ clock >= next_fire_at[p] /\ clock \in WindowOpen[p]
        /\ lock = "None" /\ ~patrol_firing[p])
        ~> patrol_firing[p]

\* ---------- Type invariant ----------

LockValues == Patrols \cup {"None"}
TimeRange  == 0..(MaxTime + 3)

TypeOK ==
    /\ clock            \in 0..MaxTime
    /\ patrol_firing    \in [Patrols -> BOOLEAN]
    /\ next_fire_at     \in [Patrols -> TimeRange]
    /\ last_completion  \in [Patrols -> 0..MaxTime]
    /\ lock             \in LockValues
    /\ sediment         \in 0..MaxSediment
    /\ cascade_detected \in BOOLEAN

\* State constraint — caps the explored depth by clock value. Without
\* this TLC explores the full Cartesian product of bookkeeping counters
\* even after clock hits MaxTime.
StateBound ==
    /\ clock <= MaxTime
    /\ \A p \in Patrols : next_fire_at[p] <= MaxTime + 3
=========================================================================
