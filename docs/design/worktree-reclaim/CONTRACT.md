# Worktree reclaim contract — issue 61

Status: working implementation contract, P2; no production behavior changes.
The two predicates below replace the contradictory single reclaim outcome.
This document and its [executable witness](../../../tests/harness/worktree-reclaim-contract.py)
are a finite specification, not a frozen production regression test. The
characterization tests already on the integration branch remain characterizations.
`affected_ref = v0.6.0` and `upstream_version = 0.6.0` are **reconstructed,
not reported**; the reporters have not supplied their version.

## Observation boundary

An injectable observation port supplies domain values; the two pure predicates
perform no filesystem, Git, process, lock, network, or clock operations (ADR-082).
Each binary question has three values: positive evidence, negative evidence,
`Unknown(error)`. Unknown is never `None`, zero, an empty list, or `false`.
Errors retain operation, candidate path, and failure cause (including command
exit/parse failure). Diagnostic payloads do not change the truth table.

The complete fixture product has these seven axes (5,832 rows):

| Axis | Values | Meaning |
|---|---|---|
| S: status | Pending, Queued, Running, Frozen, Starved, Completed, Collapsed, Unknown | Known lifecycle value or failed observation |
| R: registration | Registered, Unregistered, Unknown | Positive membership, proven nonmembership, or failed enumeration |
| L: lock | Held, Acquired, ProbeFailed | Contention, owned exclusion guard, or Unknown(error) |
| A: commits-ahead | Zero, Positive, Unknown | Proven ancestry, non-ancestry, or failed proof |
| D: dirty | Clean, Dirty, Unknown | Successful empty/nonempty status, or failed probe |
| I: ignored-durable-present | Absent, Present, Unknown | Complete inventory excluding validated derived content, or failure |
| M: molecule-absent | Present, Absent, Unknown | Found record, proven absence, or failed lookup |

For A, Zero means the *actual candidate HEAD* is reachable from the resolved
base: zero commits in base..HEAD with both refs verified. Positive counts all
map to Positive. Missing branch/base, parent-repository fallback, mismatched Git
top level, or unreadable registration cannot establish Zero. Never synthesize
`feat/<id>` for a directory without a molecule and call a missing ref merged.
Dirty includes tracked modifications and nonignored untracked paths. Ignored
content is separately inventoried: an ignored note is durable unless it lies
in the validated derived set. Failed Git status (spawn, exit, decoding, parsing)
is Unknown with its error, even if the ahead probe succeeded.

M=Absent makes S irrelevant; its eight repeated rows deliberately test that
stale status cannot become authority. M=Present/S=Unknown withholds. Unknown
on an input irrelevant to a predicate does not infect that predicate: derived
selection must remain ancestry-blind and dirt-blind even on probe errors.

## Two named predicates

`DerivedSelection(DerivedObservation { path, S, M, L, derived_set })`
returns `Keep | ReclaimDerived`. Its selected path set is either empty or the
validated derived roots under that candidate. `target/` is the default root;
a configurable derived set is required, its config spelling is not chosen here.
Registration, ancestry, dirt, and ignored durable content are not inputs.

`DurableEligibility(DurableObservation { path, S, M, R, A, D, I })`
returns `Withhold | Eligible`. Withhold means keep durable content. Eligible is
an advisory reachability result; it authorizes **no automatic removal**, even
when derived selection also succeeds. Lock is not an input: eligibility is
not execution authorization, and a harvest transaction must still enforce its
own exclusion and authorization. No new automatic path removes a worktree;
existing harvest ownership and opt-out remain. No timeout weakens a withhold.

Molecule status is a veto, never proof of reachability. The following complete
normalization table defines G, permission to consider either predicate:

| M | S | G |
|---|---|---|
| Absent | * | Yes |
| Present | Completed, Collapsed | Yes |
| Present | Pending, Queued, Running, Frozen, Starved, Unknown | No |
| Unknown | * | No |

This conservatively protects resumable/nonterminal molecules, including Frozen
and Starved. A molecule-less directory can yield derived content if the lock
is acquired. It can be advisory-eligible only with independent registration,
ancestry and content evidence; unregistered scratch cannot pass durable.

## Complete truth table and exact-set falsifiers

The tables are disjoint exhaustive row classes, not illustrative examples.
`*` expands to every value on that axis, including Unknown. Substitute the G
table above; cross the derived and durable tables to obtain every row of the
seven-axis product. The witness expands and checks all rows and can print them
with `--table`. No row is dropped as “unrealistic”.

For a singleton fixture at w, let E(w) be its validated derived-root set (the
witness uses `{w/target, w/cache}`), and let H(w) = `{w}` be its advisory durable
candidate set. Each equality below is the falsifier for that entire row class.
An automatic whole-worktree removal selection is always `{}`.

| Derived class | G | L | R,A,D,I | Derived selected set |
|---|---|---|---|---|
| d0 | No | * | * | = {} |
| d1 | Yes | Held | * | = {} |
| d2 | Yes | ProbeFailed | * | = {} |
| d3 | Yes | Acquired | * | = E(w) |

| Durable class | G | R | A | D | I | L | Durable advisory set |
|---|---|---|---|---|---|---|---|
| h0 | No | * | * | * | * | * | = {} |
| h1 | Yes | Unregistered,Unknown | * | * | * | * | = {} |
| h2 | Yes | Registered | Positive,Unknown | * | * | * | = {} |
| h3 | Yes | Registered | Zero | Dirty,Unknown | * | * | = {} |
| h4 | Yes | Registered | Zero | Clean | Present,Unknown | * | = {} |
| h5 | Yes | Registered | Zero | Clean | Absent | * | = H(w) |

The required concrete row is S=Collapsed, M=Present, R=Registered,
L=Acquired, A=Positive, D=Dirty, I=Absent: derived set **= E(w)**,
durable advisory set **= {}**. In particular it yields `w/target`.
Changing I to Present preserves those sets and preserves the ignored note.

For a batch F, falsifiers are exactly
`derived(F) = union(E(w) for rows in d3)` and
`durable(F) = {w for rows in h5}`. Paths are unique fixture identifiers;
there are no byte-count or directory-count assertions.

Two refutation modes are mandatory:

* False-red controls: for every dirty or non-ancestor row,
  `durable({w}) = {}`. On a permitting G/Acquired row the derived set still
  equals E(w). Retained durable directories must be tested against both
  production refs when the implementation seam exists.
* Differential controls with other conjuncts satisfied: Held → Acquired
  changes derived `{}` → E(w) and leaves durable `{w}` → `{w}`;
  Zero → Positive changes durable `{w}` → `{}` and leaves derived E(w) → E(w).
  Clean → Dirty and Absent → Present on I have the same durable-only effect;
  Registered → Unregistered also changes only durable. The G veto can change
  both predicates; no universal one-output-only claim is made for status.

## Lock and path obligations on the implementation

Held means another holder prevented a nonblocking exclusive flock on
`<wt>/target/debug/.cargo-lock`. Acquired means the observer actually acquired
that flock and the adapter retains its guard for the **entire reclamation**.
A missing file, inaccessible path, unsupported lock, or other failure is
ProbeFailed/Unknown and keeps derived content. Mere absence of contention or
a successful sample followed by unlock is not Acquired. Dry-run observations
cannot be reused as mutation authority: reacquire and revalidate on execution.

Holding an fd while unlinking its lock file is insufficient: another process
could create a new inode at the same name. The implementation must preserve
the lock anchor and its path identity for the protected interval, or prove an
equivalent exclusion protocol before deleting it. Selecting `target/` names
its derived payload; it does not require deleting the synchronization anchor
or promise that the root directory disappears. The finite witness proves no
filesystem race property. A real contention/replacement test is still owed.

Derived roots must be validated as rebuildable, worktree-local relative paths:
no root itself, parent traversal, Git metadata, durable override, or symlink
escape. Classification/inventory failure supplies no selected root, with a
reason; it must not silently widen the set. The matrix assumes E(w) is already
validated and present; known absent derived paths contribute the empty set.
An Acquired cargo lock alone does not exclude noncooperating producers for
custom roots: adapters must establish their applicable exclusion or withhold
those roots. Port capability and path validation are execution preconditions,
not an ancestry conjunct smuggled into DerivedSelection.

## Existing consumers: explicit migration contract

Both dirty probes currently fail open: `purge.rs::dirty_paths` deliberately
returns an empty list on failure, and the worktree-removal error arm in
`cosmon-harvest/src/transaction.rs` warns and proceeds. The earlier claim that
purge fails closed was incorrect. Neither is changed by this document.

The next implementation must share the error-preserving observation port,
with injected failures at both consumers. `purge.rs` must withhold the guarded
unharvested-work sweep decision on Unknown and surface the reason; this
contract does **not** retain its deliberate fail-open behavior. This may leave
stale worker records until observation succeeds; correctness takes precedence
over sweep progress. Known absent paths remain distinguishable from failed
probes. Existing unguarded terminal bookkeeping is not durable eligibility
and cannot authorize filesystem deletion.

`transaction.rs` must withhold worktree removal on Unknown, record the failed
operation, and preserve the worktree for retry. Generic force/allow-unharvested
must not reinterpret failed observation as Clean/Eligible. Previously completed
transaction steps need not be rolled back; partial progress must be reported
honestly. Both consumers need tests for failed status with a successful Zero
ancestry observation, plus ignored durable and molecule-less fixtures. Sharing
today's lossy helper or adding only ahead-error tests cannot meet this contract.

## Satisfiability proof and limits

Run `python3 tests/harness/worktree-reclaim-contract.py`: GREEN, exit 0,
exactly one complete output assignment satisfies the tables and refutations.
Run the same command with `--legacy-two-state`: RED, exit 1, zero assignments.
The latter substitutes Held/Absent for Held/Acquired/ProbeFailed and maps
Absent to Unknown/Keep, while retaining the required lock differential and
collapsed dirty/non-ancestor reclaim row. It fails for the old contradiction.

The script enumerates every input row and all four assignments of the two
binary outputs per row. Row constraints factor; multiplying their counts
counts all global assignments without enumerating 4^5832 vectors. It then
checks cross-row differential equalities against the unique candidate. The
legacy model has one locally determined candidate which violates the mandatory
differential, hence zero global satisfying assignments. It checks every local
assignment, not merely one hand-coded planner's success, and reads the tables
from this document so table edits cannot silently leave the witness green.

This proves finite logical consistency only. It creates no production seam,
freezes no issue-61 implementation red, runs no Git deletion, and claims no
lock protocol or performance result. The next molecule owes implementation
and adapter tests against these exact selection sets.

## Not decided here

P3 owns CLI shape/alias/defaults, evict-set config key spelling, pressure-check
placement, enumeration wiring and user-facing presentation. No CLI/UI parity
entry changes here because no command changes. An ADR may be warranted for
“no new automatic path removes a worktree”; this working document does not
ratify one. No byte threshold, age-based escape, or new removal authority is
introduced by those deferred surface choices.
