# ADR-151: Critical-task declaration is a monotone provenance fold

- Status: accepted
- Date: 2026-07-11
- Decision owner: Noogram
- Source: `delib-20260711-c6c8`, outcome C2

## Decision

Criticality is an immutable, provenance-bearing declaration about a subject
revision. Its ordered levels are `routine < root < security < max`. Each fact
records subject, revision, level, source, actor, reason, and timestamp. The
effective value is the maximum of every matching fact. Equal maxima retain all
declarers. A later lower fact is retained and diagnosed as a downgrade attempt;
it never changes the effective value.

The authoritative facts belong to the operator/fleet ledger outside the
audited worker's worktree. Project policy evaluates its baseline before worker
dispatch and appends a fact with `source = baseline`. Operators, formulas,
policies, and workers may append or raise. There is deliberately no delete,
replace, `force_routine`, or `disable_committee` operation. This satisfies
Buterin S-1 structurally: the constraint is supplied from outside the party it
constrains, while the worker can still demand stricter review.

This is an attribution and detection boundary, not a claim of tamper-proof
storage. A principal with filesystem authority can bypass it; §8b requires
that such a bypass be visible and attributable, never described as impossible.

## Projections and diagnostics

`stake:<level>` and a formula/committee `stake` variable are derived views,
never inputs to the fold. Reconciliation compares them with the fold and emits
diagnostics for:

1. a missing or different `stake:*` tag;
2. a formula stake below the effective level;
3. policy-expected criticality with no declaration;
4. `root`, `security`, or `max` without a linked cross-provider committee; and
5. every retained downgrade attempt, including its actor and reason.

Downstream committee wiring consumes the effective level and provenance. It
must not infer authority from an editable tag or from the worker's formula
variables.

## Consequences

The pure schema, fold, and projection diagnostics live in
`cosmon_core::criticality`. Persistence and UI shells can share that kernel
without duplicating ordering or weakening provenance. Legacy tasks with no
declaration fold to `routine`, but a policy that expected classification makes
that absence visible rather than silently blessing it.
