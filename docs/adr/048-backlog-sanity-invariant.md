# ADR-048: Backlog-Sanity Invariant of the Autonomous Regime

**Status:** Proposed
**Date:** 2026-04-17
**Parent idea:** `idea-20260417-ba1d`
**Amends by reference:** [ADR-016](016-autonomy-regimes-and-resident-runtime.md)
**Related:** [ADR-022](022-native-dag-scheduler.md) (native DAG scheduler),
[ADR-038](038-runtime-adaptive-scope.md) (runtime adaptive scope),
[ADR-041](041-atomic-frontier-projection.md) (atomic frontier)

## Context

On 2026-04-17 (evening), during *Opération Executor* on the `mailroom`
galaxy, two `cs tackle` invocations on DAG roots (`78bf`, `87cd`) were
auto-upgraded to runtime-mode (`cs run`). The resident runtime then
**resurrected 14+ pending molecules from 2026-04-14** that had been
sedimenting in the backlog without any `temp:*` tag. Roughly 40 min of
worker cycles were burned on zombie work before the operator killed the
tmux sessions and re-tackled with `cs tackle --leaf` to bypass the
runtime path.

This is the *convoy cascade* pathology chronicled in
an internal chronicle:

> State is not what is. State is what has accumulated. [...] Cosmon
> mechanisms must be scoped; greedy defaults amplify silent state
> accumulation into runaway cascades.

The original 2026-04-12 incident produced `task-20260412-30c1` (scope
repair of the runtime DAG walker). That repair closed the specific
backward-edge traversal bug but did not address the **general** class:
*any* runtime bootstrap against a sedimented backlog remains at risk when
the walker is allowed to reach pending molecules whose `updated_at` is
days old. The April 17 incident proved the class is not empty.

### Existing partial defense

A target-level guard already exists:
[`warn_if_stale_untagged`](../../crates/cosmon-cli/src/cmd/guard.rs)
prints a stderr nag when the **direct target** of `cs tackle` is pending,
older than `STALE_PENDING_HOURS = 2`, and lacks a `temp:*` tag. This is
warn-level only, inspects only the direct target, and never fires when
the target itself is fresh — which is exactly what happened on April 17:
`78bf` and `87cd` were freshly nucleated, so the guard stayed silent
while the runtime swept past them into 3-day-old untagged pendings.

The existing guard is necessary but **not sufficient** for the
runtime-mode path. The `--leaf` manual bypass is a workaround, not a
solution — it forces the operator to know, in the moment, that the
backlog is dirty.

### Principle — *the reactor learns from what it burns*

This ADR is the inaugural instance of the cosmon-ward feedback loop
inscribed in an internal chronicle (entry *2026-04-17 nuit — Le
réacteur apprend de ce qu'il brûle*):

> Quand le pilotage d'une application-site (mailroom, showroom)
> révèle une friction, une pathologie, un besoin de primitive
> manquante — la surface la plus proche de l'incident est l'application,
> mais *la surface qui doit apprendre est cosmon*. Le pilote ne doit pas
> réparer silencieusement dans l'application ce qui devrait être
> redesigné dans le cœur.

If cosmon ignores this signal, the principle is decorative. If it
produces a binding decision that mechanically closes the pathology, the
principle is enforced structurally. This ADR is that decision.

## Decision

### 1. Backlog-sanity is an invariant of the Autonomous regime

[ADR-016](016-autonomy-regimes-and-resident-runtime.md) defines the
Autonomous regime as the one in which the clock and the observer live
**inside the resident runtime**. Its deliberation function (policy) is
what determines the next action. ADR-016 explicitly left runtime scope
discipline as future work.

This ADR closes that gap by **amending ADR-016 §2 (autonomy regimes) by
reference**: the Autonomous regime is constrained by the following
invariant.

> **Backlog-Sanity Invariant.** The resident runtime must not bootstrap
> onto a *dirty backlog* — a state-store whose sediment cardinality
> exceeds a configured threshold — unless the operator provides an
> explicit, audit-logged override.

This is strictly stronger than "a flag on `cs tackle`". It means: *no
DAG walk may be initiated from a state-store whose untagged-pending
cardinality exceeds the threshold, regardless of which command invoked
the runtime*. The concrete guard (pending + age + untagged) is one
mechanical expression of that invariant; `temp-review`-produced tags
are another; a future retention/GC policy could be a third.

### 2. Sediment predicate (formal)

Let the state-store's molecule set be $\mathcal{M}$ with each molecule
$m$ carrying a status, an `updated_at` timestamp, and a tag set
$\mathrm{tags}(m)$.

A molecule $m$ is **sediment** iff

$$
m.\mathrm{status} \in \{\mathsf{Pending},\, \mathsf{Queued}\}
\;\wedge\;
\mathrm{age}(m) > 48\,\mathrm{h}
\;\wedge\;
\neg\,\exists\, t \in \mathrm{tags}(m) : t.\mathrm{key} = \texttt{"temp"}.
$$

The **sediment set** is
$\mathcal{S}(\mathcal{M}) = \{\, m \in \mathcal{M} : m\text{ is sediment}\,\}$.

A backlog is **dirty** iff

$$
|\mathcal{S}(\mathcal{M})| \geq N
$$

where $N$ is a configurable integer. Default $N = 5$, overridable via
the environment variable `COSMON_RUNTIME_GUARD_STALE_THRESHOLD`.

#### Why these parameters

- **48 h**. Aligns with the existing CLAUDE.md §*Molecule Temperature
  Tags* rule: *"any `pending` molecule with `updated_at > 48h` and no
  `temp:*` tag is a candidate for re-evaluation"*. Reusing the same
  constant keeps one mental model across the curation discipline and
  the runtime guard. The existing `STALE_PENDING_HOURS = 2` in
  [`guard.rs`](../../crates/cosmon-cli/src/cmd/guard.rs) is tuned for
  *target freshness* on direct dispatch and intentionally remains its
  own constant — the runtime-scope question is different.
- **Absence of any `temp:*` tag** (not "absence of a specific tag").
  Curated `temp:cold` or `temp:frozen` pendings do not count as
  sediment — they have been inspected and parked intentionally. The
  invariant fires only on *unreflected* accumulation, which is the
  pathology.
- **N = 5**. A threshold tuned to the observed pathology (14 zombies
  on April 17, 13 on April 12) while leaving headroom for normal
  workflow noise (a handful of untagged pendings between tackle and
  the next temp-review sweep). The env override lets operators tune
  per-galaxy — mature repos with different sediment tolerances can
  raise it.
- **Pending + Queued**. Both carry the semantics "not started, waiting
  for action". `Queued` is included for the same reason `Pending` is:
  a greedy walker treats them identically as ready-to-dispatch nodes.

### 3. Reaction — option (b) refuse + `--force-runtime` is canonical

Three candidate reaction modes were evaluated in
`idea-20260417-ba1d` (see `evaluate.md` §1.2). This ADR adopts option
**(b) refuse with non-zero exit code + `--force-runtime` override**
as canonical, and explicitly rejects options (a) and (c).

| Dimension | (a) Warn + prompt | **(b) Refuse + `--force-runtime`** | (c) Auto-downgrade to leaf |
|---|---|---|---|
| Scriptable | ❌ (prompt hangs non-TTY) | ✅ (distinct exit code) | ✅ |
| Silent policy change | ❌ (proceeds on Enter) | **✅ (never silent)** | ❌ (semantic shift) |
| IFBDD operator friction | Medium (Enter key) | Low (one flag) | None — but hides the problem |
| CI / automation safe | ❌ unless TTY-gated | **✅** | ✅ |
| Teaches the operator | Medium | **✅ High (error names the fix)** | ❌ (invisible) |
| Audit trail | Warning in stderr | **Exit code + events.jsonl** | Warning only |
| Consistency with `GuardError` family (b22c, f4e1) | ❌ | **✅** | ❌ |
| Aligned with ADR-016 (policy → refusal, not warning) | No | **Yes** | No |

#### Rationale for (b) — canonical

1. **Symmetric with the existing `GuardError` family.** The type-tightening
   GuardErrors introduced in b22c and f4e1 both refuse on violation. A
   new guard that *warns* instead of refusing would be a lone exception
   in an otherwise consistent typology.
2. **Scriptable.** A distinct exit code (`DIRTY_BACKLOG_REFUSAL = 12`)
   lets external schedulers, CI, and `patrol --propel` branch
   deterministically. No TTY assumptions, no string-matching on stderr.
3. **Consistent with ADR-016's regime model.** The Autonomous regime
   *has* policy invariants. Violations are **refusals**, not warnings —
   that is how "the runtime has a policy" is distinguished from "the
   runtime has suggestions".
4. **IFBDD-friendly override.** The `--force-runtime` flag plus an
   `events.jsonl` audit entry is the explicit, auditable "I know what
   I'm doing" escape valve. No silent behavior; every override leaves
   a durable trace.
5. **The error message teaches.** The canonical refusal points the
   operator at `temp-review` and `--force-runtime` in prose. The first
   encounter is where the operator learns the curation discipline.

#### Rationale for rejecting (a) — warn + prompt

- **Non-TTY hang risk.** Interactive prompts break `patrol --propel`,
  CI, and any scripted batch invocation. `atty`-gating the prompt only
  shifts the problem: in non-TTY contexts the guard must still *do
  something*, and "default to refuse" is then equivalent to (b) with
  extra machinery for the TTY path.
- **Silent-on-Enter failure mode.** An operator who presses Enter
  without reading the warning gets exactly the cascade the guard was
  meant to prevent. The UX optimizes for the wrong failure mode.
- **Implicit statelessness violation.** Cosmon's transactional core is
  explicitly stateless and scriptable (see CLAUDE.md §Architectural
  Discipline, invariant 1). An interactive process is not one-shot.

#### Rationale for rejecting (c) — auto-downgrade to leaf

- **Silent semantic change.** The operator asked for runtime-mode (DAG
  orchestration) and silently got leaf-mode (single worker). If the
  target has 30 `Blocks` descendants, the operator will discover later
  — possibly far later — that only one child ran.
- **Hides the sediment problem.** Option (c)'s zero-friction is
  antipedagogic: the operator never learns the backlog is dirty, so
  the sediment keeps growing, so the next runtime-mode invocation
  hits the same silent downgrade. The pathology becomes steady-state
  rather than being named and fixed.
- **Violates the "no silent policy change" rule.** An operator who
  gets leaf-mode when they asked for runtime-mode loses the guarantee
  that `cs tackle` dispatched the whole DAG.

### 4. `--force-runtime` — the audited override

The `--force-runtime` flag is available on `cs tackle` and `cs run`.
When passed, the guard is bypassed **and an event is written** to
`.cosmon/state/events.jsonl`:

```json
{
  "event_type": "runtime_guard_override",
  "ts": "<ISO-8601>",
  "caller": "cs tackle" | "cs run",
  "molecule_id": "<root-id>",
  "sediment_count": <N>,
  "threshold": <N_threshold>,
  "sample": ["<mol-id>", "<mol-id>", ...]
}
```

This matches the IFBDD discipline: the operator can override the policy,
but the override is **visible** — durably, in the append-only event log,
with enough context (count, sample, threshold) that a future audit can
reconstruct the decision. Overrides leave a trail; they do not erase
the rule.

### 5. Canonical refusal UX

The refusal message is the primary UX surface of this invariant. It
must name the pathology, the exit code's meaning, and the three
remediations **in one block**, in order of preferred use:

```text
cs tackle: backlog contains {count} pending molecules older than
48 h without a temp:* tag ({sample_preview}). Running the resident
runtime would risk resurrecting them (convoy cascade, 2026-04-12).

Fix with:
  cs nucleate temp-review && cs tackle <id>     # curate the backlog
  cs tag <mol_id> --add temp:frozen             # tag individually
  cs tackle <id> --force-runtime                # override (audited)

See docs/adr/048-backlog-sanity-invariant.md.
```

Properties of this message:

- Names the problem (*pending, 48 h, no temp:\* tag*) in plain words.
- Cites the historical artifact that predicted the pathology.
- Lists three remediations in *preferred* order: curate (best), tag
  individually (surgical), override (escape hatch).
- Links to this ADR so the operator can read the invariant, not just
  obey it.

`{sample_preview}` is a comma-separated list of up to 5 molecule IDs
(truncated with `, ...` suffix if more). The full list is available
via the exit-code-branching caller's own query (`cs ensemble --status
pending --age-gt 48h --no-temp`).

### 6. Scope — both entry points

The invariant fires at **runtime bootstrap**, which means both:

- `cs tackle <root>` when auto-upgrading to runtime-mode (the incident
  path — non-leaf, non-dry-run, non-empty `Blocks`).
- `cs run <root>` when invoked directly (the power-user path).

Implementation note (from `evaluate.md` §1.4): the guard lives in the
**runtime bootstrap** (a single function called by both entry points),
not at `cs tackle`'s DAG-root detection. This preserves the coherence
invariant *"single perimeter — not duplicating an existing command's
role"*.

### 7. What this ADR does not change

- The existing `warn_if_stale_untagged` target-level guard stays as-is.
  It remains useful for direct-dispatch freshness; this ADR adds a
  *complementary* runtime-scope guard with a different question.
- The runtime DAG walker's scope logic (`refresh_scope` in
  [`dag_policy.rs`](../../crates/cosmon-runtime/src/dag_policy.rs)) is
  not re-architected here. That was `task-20260412-30c1`. This ADR adds
  a **precondition** checked before the walker runs; it does not change
  how the walker traverses once it is running.
- No change to `cosmon-core`, state-store on-disk format, molecule-id
  scheme, or public API.

## Coherence checklist

Per CLAUDE.md §*Architectural Discipline*:

| # | Invariant | Status |
|---|---|---|
| 1 | Stateless? | ✅ Guard is a pure function of `list_molecules`. No daemon, no background loop. |
| 2 | Idempotent? | ✅ Re-running the invocation produces the same refusal or the same override event. |
| 3 | Regime-aware? | ✅ Fires at the Propelled→Autonomous boundary; does not affect Inert. |
| 4 | Single perimeter? | ✅ Guard lives in `cosmon-runtime`; both entry points call it once. |
| 5 | Symmetric undo? | ✅ `--force-runtime` + `cs tag` + `temp-review` are the documented overrides. |
| 6 | Runtime-compatible? | ✅ This *is* the runtime invariant, not a CLI afterthought. |
| 7 | Worker/human boundary? | ✅ Guard runs at pilot-invocation time; workers never hit it. |
| 8 | Write-read asymmetry? | ✅ The precheck is pure read; only the override path writes an event. |
| 9 | Merge-before-dispatch? | N/A — guard fires before any dispatch. |
| 10 | CLI-first for workers? | N/A — workers do not bootstrap runtimes. |

## Re-evaluation criteria

Re-open this ADR if, after **4 weeks** of `temp:warm` observation:

- **Signal A (too strict).** `--force-runtime` is invoked on more than
  **30 %** of `cs tackle` runtime-mode dispatches → guard is over-firing;
  consider relaxing the threshold or adopting option (a) as a softer
  default for specific thresholds.
- **Signal B (insufficient).** Convoy-cascade-class incidents recur
  despite the guard → threshold or detection metric is wrong; re-examine
  the sediment predicate (add ratio-to-total? event density? different
  age cutoff?).
- **Signal C (bad UX).** Operators report surprise, confusion, or
  misdirection from the refusal message → improve error text before
  relaxing semantics. A puzzled operator is a teaching opportunity, not
  a reason to loosen the rule.

## Consequences

**Positive:**

- The convoy-cascade pathology has a mechanical, scoped, auditable
  prevention that is *visible* at every dispatch. No more silent
  accumulation.
- The Autonomous regime gains its first formally-specified invariant,
  closing a gap left by ADR-016. Future Autonomous-regime invariants
  (retention, decay, recovery) can follow the same pattern: predicate
  + threshold + refusal-with-override + audit event.
- The refusal UX teaches the `temp-review` discipline at exactly the
  moment an operator encounters a dirty backlog — the on-ramp is
  moved from folklore to error-message prose.
- First complete cycle of *le réacteur apprend de ce qu'il brûle*:
  an application-site incident (mailroom) produced a
  cosmon-ward signal, an idea-to-plan molecule, an evaluation, a
  canonical plan, and a binding ADR. The principle has turned once,
  end-to-end.

**Negative:**

- Stricter than the status quo. An operator who *wants* to run a
  possibly-dirty backlog must either `cs tag` offenders, nucleate a
  `temp-review`, or pass `--force-runtime`. For a backlog of 14
  offenders this is 14 `cs tag` calls (or one `--force-runtime` +
  events.jsonl trail).
- Adds a new exit code (`12`) and a new `GuardError` variant. External
  tools that exec `cs` and inspect exit codes must learn about it.

**Neutral:**

- The walker itself is unchanged. The invariant is a **precondition**,
  not a behavior-modification of the existing DAG traversal. The two
  concerns stay separable.

## Implementation task

The implementation is tracked by `task-20260417-d798`
(`cs tackle runtime-mode — implement backlog-sanity guard`). Scope
summary (full detail lives in that molecule's formula steps and in
`idea-20260417-ba1d/plan.md` §*Child 2*):

1. `crates/cosmon-runtime/src/guard.rs` — new module with
   `check_backlog(store, force: bool) -> Result<(), RuntimeGuardError>`
   and `SedimentReport { count, sample }`.
2. `GuardError::DirtyBacklogRuntimeRefusal { count, sample }` +
   `exit_code::DIRTY_BACKLOG_REFUSAL = 12` in
   `crates/cosmon-cli/src/cmd/guard.rs`.
3. CLI wiring in `crates/cosmon-cli/src/cmd/tackle.rs` (runtime-mode
   branch) and `crates/cosmon-cli/src/cmd/run.rs` (bootstrap).
4. `--force-runtime` flag on `cs tackle` and `cs run`; emits
   `runtime_guard_override` event when used.
5. Tests — unit (`check_backlog` on clean/dirty/force), integration
   (convoy-cascade regression: 5 untagged pendings >48 h, assert exit
   code 12 and no runtime spawned), proptest on threshold boundary.
6. Chronicle update after merge: close the *réacteur apprend* loop in
   an internal chronicle with an entry linking idea → ADR → impl.

## References

- **Parent idea**: `idea-20260417-ba1d` (`capture.md`, `evaluate.md`,
  `plan.md`). Option (b) comparison table from `evaluate.md` §1.2;
  canonical recommendation from `evaluate.md` §5; child molecule
  scopes from `plan.md` §*Implementation plan*.
- **Predictive chronicle**: an internal chronicle
  — original incident that named the pathology.
- **Inaugural chronicle**: an internal chronicle
  §*2026-04-17 nuit — Le réacteur apprend de ce qu'il brûle* —
  cosmon-ward feedback loop principle of which this ADR is the first
  instance.
- **Incident report**: an internal mailroom note
  §*Note de contexte* under principle §8.
- **Amended**: [ADR-016](016-autonomy-regimes-and-resident-runtime.md)
  §2 — autonomy regimes; this ADR adds the backlog-sanity invariant
  to the Autonomous regime.
- **Related runtime scope work**: `task-20260412-30c1` (walker scope
  repair, closed), [ADR-038](038-runtime-adaptive-scope.md) (runtime
  adaptive scope).
- **Existing partial guard**: [`crates/cosmon-cli/src/cmd/guard.rs`](../../crates/cosmon-cli/src/cmd/guard.rs)
  `warn_if_stale_untagged` — target-level freshness (stays, complements
  this invariant).
- **Resurrection mechanism**: [`crates/cosmon-runtime/src/dag_policy.rs`](../../crates/cosmon-runtime/src/dag_policy.rs)
  `refresh_scope` — walker whose dispatch is now gated by the guard.
- **CLAUDE.md §Molecule Temperature Tags** — the curation discipline
  this invariant enforces mechanically.
