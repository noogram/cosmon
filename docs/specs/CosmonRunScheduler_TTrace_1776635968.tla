---- MODULE CosmonRunScheduler_TTrace_1776635968 ----
EXTENDS Sequences, TLCExt, Toolbox, Naturals, TLC, CosmonRunScheduler

_expression ==
    LET CosmonRunScheduler_TEExpression == INSTANCE CosmonRunScheduler_TEExpression
    IN CosmonRunScheduler_TEExpression!expression
----

_trace ==
    LET CosmonRunScheduler_TETrace == INSTANCE CosmonRunScheduler_TETrace
    IN CosmonRunScheduler_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        last_completion = (("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0))
        /\
        cascade_detected = (TRUE)
        /\
        patrol_firing = (("patrol-propel" :> TRUE @@ "purge-stale" :> FALSE @@ "nightly-drain" :> FALSE @@ "temp-review" :> FALSE @@ "backlog-sanity" :> FALSE))
        /\
        lock = ("patrol-propel")
        /\
        clock = (0)
        /\
        next_fire_at = (("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0))
        /\
        sediment = (1)
    )
----

_init ==
    /\ lock = _TETrace[1].lock
    /\ sediment = _TETrace[1].sediment
    /\ patrol_firing = _TETrace[1].patrol_firing
    /\ next_fire_at = _TETrace[1].next_fire_at
    /\ clock = _TETrace[1].clock
    /\ last_completion = _TETrace[1].last_completion
    /\ cascade_detected = _TETrace[1].cascade_detected
----

_next ==
    /\ \E i,j \in DOMAIN _TETrace:
        /\ \/ /\ j = i + 1
              /\ i = TLCGet("level")
        /\ lock  = _TETrace[i].lock
        /\ lock' = _TETrace[j].lock
        /\ sediment  = _TETrace[i].sediment
        /\ sediment' = _TETrace[j].sediment
        /\ patrol_firing  = _TETrace[i].patrol_firing
        /\ patrol_firing' = _TETrace[j].patrol_firing
        /\ next_fire_at  = _TETrace[i].next_fire_at
        /\ next_fire_at' = _TETrace[j].next_fire_at
        /\ clock  = _TETrace[i].clock
        /\ clock' = _TETrace[j].clock
        /\ last_completion  = _TETrace[i].last_completion
        /\ last_completion' = _TETrace[j].last_completion
        /\ cascade_detected  = _TETrace[i].cascade_detected
        /\ cascade_detected' = _TETrace[j].cascade_detected

\* Uncomment the ASSUME below to write the states of the error trace
\* to the given file in Json format. Note that you can pass any tuple
\* to `JsonSerialize`. For example, a sub-sequence of _TETrace.
    \* ASSUME
    \*     LET J == INSTANCE Json
    \*         IN J!JsonSerialize("CosmonRunScheduler_TTrace_1776635968.json", _TETrace)

=============================================================================

 Note that you can extract this module `CosmonRunScheduler_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `CosmonRunScheduler_TEExpression.tla` file takes precedence 
  over the module `CosmonRunScheduler_TEExpression` below).

---- MODULE CosmonRunScheduler_TEExpression ----
EXTENDS Sequences, TLCExt, Toolbox, Naturals, TLC, CosmonRunScheduler

expression == 
    [
        \* To hide variables of the `CosmonRunScheduler` spec from the error trace,
        \* remove the variables below.  The trace will be written in the order
        \* of the fields of this record.
        lock |-> lock
        ,sediment |-> sediment
        ,patrol_firing |-> patrol_firing
        ,next_fire_at |-> next_fire_at
        ,clock |-> clock
        ,last_completion |-> last_completion
        ,cascade_detected |-> cascade_detected
        
        \* Put additional constant-, state-, and action-level expressions here:
        \* ,_stateNumber |-> _TEPosition
        \* ,_lockUnchanged |-> lock = lock'
        
        \* Format the `lock` variable as Json value.
        \* ,_lockJson |->
        \*     LET J == INSTANCE Json
        \*     IN J!ToJson(lock)
        
        \* Lastly, you may build expressions over arbitrary sets of states by
        \* leveraging the _TETrace operator.  For example, this is how to
        \* count the number of times a spec variable changed up to the current
        \* state in the trace.
        \* ,_lockModCount |->
        \*     LET F[s \in DOMAIN _TETrace] ==
        \*         IF s = 1 THEN 0
        \*         ELSE IF _TETrace[s].lock # _TETrace[s-1].lock
        \*             THEN 1 + F[s-1] ELSE F[s-1]
        \*     IN F[_TEPosition - 1]
    ]

=============================================================================



Parsing and semantic processing can take forever if the trace below is long.
 In this case, it is advised to uncomment the module below to deserialize the
 trace from a generated binary file.

\*
\*---- MODULE CosmonRunScheduler_TETrace ----
\*EXTENDS IOUtils, TLC, CosmonRunScheduler
\*
\*trace == IODeserialize("CosmonRunScheduler_TTrace_1776635968.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE CosmonRunScheduler_TETrace ----
EXTENDS TLC, CosmonRunScheduler

trace == 
    <<
    ([last_completion |-> ("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0),cascade_detected |-> FALSE,patrol_firing |-> ("patrol-propel" :> FALSE @@ "purge-stale" :> FALSE @@ "nightly-drain" :> FALSE @@ "temp-review" :> FALSE @@ "backlog-sanity" :> FALSE),lock |-> "None",clock |-> 0,next_fire_at |-> ("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0),sediment |-> 0]),
    ([last_completion |-> ("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0),cascade_detected |-> FALSE,patrol_firing |-> ("patrol-propel" :> FALSE @@ "purge-stale" :> FALSE @@ "nightly-drain" :> FALSE @@ "temp-review" :> FALSE @@ "backlog-sanity" :> FALSE),lock |-> "None",clock |-> 0,next_fire_at |-> ("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0),sediment |-> 1]),
    ([last_completion |-> ("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0),cascade_detected |-> TRUE,patrol_firing |-> ("patrol-propel" :> TRUE @@ "purge-stale" :> FALSE @@ "nightly-drain" :> FALSE @@ "temp-review" :> FALSE @@ "backlog-sanity" :> FALSE),lock |-> "patrol-propel",clock |-> 0,next_fire_at |-> ("patrol-propel" :> 0 @@ "purge-stale" :> 0 @@ "nightly-drain" :> 0 @@ "temp-review" :> 0 @@ "backlog-sanity" :> 0),sediment |-> 1])
    >>
----


=============================================================================

---- CONFIG CosmonRunScheduler_TTrace_1776635968 ----
CONSTANTS
    MaxTime = 2
    MaxSediment = 1
    S3Enabled = FALSE
    AutopilotEnabled = FALSE
    BacklogThreshold = 2

INVARIANT
    _inv

CHECK_DEADLOCK
    \* CHECK_DEADLOCK off because of PROPERTY or INVARIANT above.
    FALSE

INIT
    _init

NEXT
    _next

CONSTANT
    _TETrace <- _trace

ALIAS
    _expression
=============================================================================
\* Generated on Sun Apr 19 23:59:28 CEST 2026