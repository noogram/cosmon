# ADR-040 — Runtime ↔ Cognition Architecture (draft)

- **Status**: Proposed
- **Date**: 2026-04-14
- **Deliberation**: delib-20260414-2ab2 (9-persona panel, see synthesis below)
- **Supersedes**: n/a (clarifies runtime/cognition coupling implicit since ADR-016)
- **Relates**: ADR-016 (autonomy regimes), ADR-039 (fleet composability), delib-e6b8 (formal spec, deferred)

## Formal invariants (reference block)

Every phase MUST preserve these. Phase 3 makes them structurally impossible
to violate; Phases 1–2 enforce them at the data layer.

### Observer invariants (derived from the panel)

- **I1**  `1 molecule = 1 observable lifecycle` — `cs observe M` returns one coherent status.
- **I2**  `1 molecule = 1 entry in cs peek` — no visual duplication by role.
- **I8**  `Crash atomicity` — runtime and worker deaths are correlated events
          of one lifecycle, not independent failures to reconcile.
- **I9**  `Observer uniqueness` — the operator's mental model has exactly one
          handle per molecule, regardless of how many processes serve it.

Violation of any of I1/I2/I8/I9 is the signature of the current bug.

### Composition rule (Gödel, delib-778e pre-emptive)

```
S(m) := molecule_id(m)              — session name is the molecule id
Σ(m) := root_fleet_id(m)            — socket namespace is the ROOT fleet
```

`Σ(m)` is constant along any composition tree: a master fleet does NOT create
a new socket when dispatching into a child fleet. Only new sessions. This is
the invariant that makes ADR-039 (fleet composability) recursively decidable
without a joint state machine across nested runtimes.

### Coupling invariants (Hawking, Phase 3 only)

These become *structural* under split-window and are the ones a TLA+/foundry
spec (delib-e6b8) should bite on:

| ID  | Statement                                                                                                               |
|-----|-------------------------------------------------------------------------------------------------------------------------|
| R1  | `tmux has-session -t <slug>` ⇔ `fleet.json[mol_id].desired = running`. Divergence → `cs patrol` collapse or respawn.    |
| R2  | `pane-died` hook on either pane → `kill-session`. Cognition death kills the session, which kills the runtime. Symmetric.|
| R3  | Intra-session watchdog (30 s). Runtime pane runs `cs run --poll-interval 5 --guard-cognition <pane_id>`; on `pane_dead && desired=running`, respawn or `cs collapse`. |
| R4  | `cs done <mol>` = `tmux kill-session` + merge + teardown, atomic.                                                        |
| R5  | `cs patrol` anomaly signal = `tmux list-panes -t <slug> \| wc -l == 2 && !any(pane_dead)`. One-bit, O(1).                |

## Context

When `cs tackle` is run on a molecule M that has outgoing `Blocks` edges,
cosmon today spawns **two tmux sessions** inside the same project socket:

- `runtime-<slug>` — runs `cs run M`, polls the DAG frontier, dispatches
  children.
- `<slug>` — runs a Claude worker, performs cognition for M itself.

Observed on 2026-04-14 (delib-20260414-2ab2):

1. **Operator confusion.** Attaching to `runtime-<slug>` shows the polling
   loop, not the worker. There is no visual cue that the cognition lives
   in a sibling session.
2. **Crash desync.** When the cognition session dies while `desired=running`,
   the runtime session keeps polling indefinitely; molecule status remains
   `running` with no one advancing it.
3. **`fleet.json` doublons.** A leaf worker and a runtime worker can end up
   referencing the same `mol_id` from two separate entries, so `cs peek`
   displays the molecule twice.
4. **`tl` illegibility.** Runtimes, active workers, and diverged workers
   all share the same socket namespace with no role hierarchy.

The root cause, per the panel synthesis, is a **broken bijection** between
molecule and observable entity. `fleet.json` carries no discriminant for
role, `cs peek` cannot dedupe, `cs patrol` cannot detect a "phantom
runtime" with a simple signal, and the operator's mental model is forced
to reconcile two surfaces for one object.

## Decision

Adopt a **three-phase migration** toward a one-session-per-molecule model,
driven by the information-theoretic observation that the current `fleet.json`
schema lacks **one bit of role semantics**.

### Invariants (to preserve through all phases)

- **I1** `1 molecule = 1 observable lifecycle` — `cs observe M` returns one
  coherent status.
- **I2** `1 molecule = 1 entry in cs peek` — no visual duplication.
- **I8** `Crash atomicity` — runtime and worker death are correlated events
  of one lifecycle, not independent failures to reconcile.
- **I9** `Observer uniqueness` — the operator's mental model has one handle
  per molecule.

Violation of any of I1/I2/I8/I9 is the signature of the current bug.

### Phase 1 — `WorkerRole` in `fleet.json` (immediate, additive)

Add a single field to each worker entry:

```rust
enum WorkerRole { Cognition, Runtime }

struct WorkerEntry {
    mol_id: MolId,
    status: WorkerStatus,
    desired: DesiredState,
    role: WorkerRole,       // NEW — default Cognition for backward-compat
    socket: SocketPath,
    session: SessionName,
    parent: Option<WorkerId>,
    // ... existing fields unchanged
}
```

**Rationale.** The current schema cannot distinguish runtime from cognition
anywhere on disk, which is why the three bugs (doublons, confused purge,
undetectable phantom) are all possible simultaneously. A 1-bit field resolves
it. `cs reconcile` repopulates `role` from the current heuristic (presence
of `cs run` in the session) — no operator action required.

**Downstream consequences**:

- `cs peek` fuses rows by `mol_id`, default-filtering to `Cognition`, with
  a `--role runtime|all` flag for plumbing views.
- `cs purge` only removes a `Runtime` worker when all its children (workers
  with `parent == runtime.wid`) are terminal. Prevents orphaning.
- `cs patrol` detects anomalies with a single query: "for each `running`
  mol, is there exactly one `Cognition` worker alive?".
- `cs ensemble` aggregates cleanly: `count(Cognition)` = work in flight,
  `count(Runtime)` = active orchestrators.

**Semver.** Additive minor bump. Old state files deserialize with default
`role = Cognition`.

### Phase 2 — Opt-in `socket_scheme = "split_by_role"` (mitigation)

```toml
[transport]
socket_scheme = "single"          # default — byte-compatible with pre-ADR-040 fleets
# socket_scheme = "split_by_role" # opt-in — runtimes go to cosmon-runtimes-<slug>
```

**Rationale.** An interim mitigation for operators who want namespaced
visibility immediately without waiting for Phase 3. Pure naming convention:
the parameterized tmux socket name changes, no schema change, no
breaking surface. Resolves symptoms 1 and 4 (operator confusion, `tl`
illegibility) but not the underlying bijection violation.

**Prerequisite audit.** `cs peek --all` socket discovery regex must tolerate
multiple sockets per project (`cosmon-<slug>` **and** `cosmon-runtimes-<slug>`).
This fix is **required before** Phase 2 ships, otherwise `--all` becomes
silently incomplete. Files to audit: `crates/cosmon-cli/src/cmd/peek.rs`,
`crates/cosmon-transport/`.

**Semver.** Additive minor bump. Default stays `"single"`. No migration.

### Phase 3 — Split-window: one tmux session per molecule (target)

The structural fix. When `cs tackle` dispatches a molecule M that has
`Blocks` outgoing edges, spawn **one tmux session** `<slug>` with **two
panes**:

- **Left pane** — `cs run <mol_id>` (runtime polling).
- **Right pane** — Claude worker (cognition).

**Formal rule (from Gödel).** `S(m) := molecule_id(m)`. Never two session
names per molecule. `Σ(m) := root_fleet_id(m)` — one socket per project,
constant along any composition tree (hard dependency for ADR-039 fleet
composability: a master fleet never creates a new socket, only new sessions).

**Coupling invariants (from Hawking).**

- **R1** `tmux has-session -t <slug>` ⇔ `fleet.json[mol_id].desired = running`.
  Divergence → `cs patrol` collapse or respawn.
- **R2** `pane-died` hook on both panes → `kill-session`. Cognition death
  kills the whole session, which kills the runtime. Symmetric.
- **R3** Intra-session watchdog (30s). Runtime pane runs
  `cs run --poll-interval 5 --guard-cognition <pane_id>`; on `pane_dead && desired=running`,
  respawn (retries < N) or `cs collapse <mol> --reason cognition-died`.
- **R4** `cs done <mol>` = `tmux kill-session` + merge + teardown, atomic.
- **R5** `cs patrol` anomaly signal = `tmux list-panes -t <slug> -F ... ; count == 2 && !any(pane_dead)`. Binary, O(1).

**What this buys**:

- Bijection session ↔ molecule (I1, I2, I9 structurally guaranteed).
- Crash atomicity (I8 via `pane-died` hook — no more runtime-polling-dead-worker).
- `cs patrol` with a one-bit detection signal.
- Zero orphan resources after `cs done`.

**Semver.** Minor bump with opt-out (`socket_scheme = "legacy_two_sessions"`)
for the grace window. Old fleets in flight continue on the legacy scheme;
new fleets default to split-window. Grace window: ~4 weeks, then `legacy_two_sessions`
becomes `deny-by-default` with a migrator.

**Out of scope for Phase 3** (future work):

- Runtime daemon (Option 4) — requires resident runtime, see ADR-016 Phase 3+.
- Runtime fused into worker (Option 5) — rewrites regime purity (I10),
  would require a successor ADR to ADR-016 before being adopted.

## Rejected alternatives

| Option | Why rejected |
|---|---|
| **Status quo** (Option 1) | Violates I1, I2, I8, I9. Every observed bug is a symptom of this geometry. |
| **Two sessions, distinct sockets by default** (Option 2 non-opt-in) | Breaking silencieux: `tl`, `cs peek --all`, operator scripts all grep `cosmon-<slug>` patterns. Adopted as opt-in only (Phase 2). |
| **Runtime daemon without tmux** (Option 4) | Violates the Transactional Core invariant "never introduce a daemon" (architectural-invariants.md). Deferrable to ADR-016 Phase 3+ Resident Runtime. |
| **Runtime fused into worker** (Option 5) | Rewrites I10 (regime purity), couples control and data planes, makes a cognition OOM kill all child dispatches. Paradigm shift, not a refactor — requires its own ADR. |

## Operator UX target

> **tenant_auditor, opératrice naïve, tape `cs tackle <delib>` sur une molécule avec un
> enfant. Elle veut voir la cognition. Critère de succès : ≤3 actions, zéro
> question, pas de risque de tuer le runtime par erreur.**

- **Phase 1 alone**: `cs peek` shows one line per molecule, tenant_auditor opens the
  detail view with `p`. 3 actions, no confusion, but `tl` still shows two
  sessions.
- **Phase 2 (opt-in)**: `tl` on the project socket shows only the cognition;
  runtimes live in `cosmon-runtimes-<slug>`. tenant_auditor can't accidentally attach
  to the runtime. Still two sessions in the process tree.
- **Phase 3**: tenant_auditor sees one session `<slug>`. Attaches, sees Claude on the
  left pane and the runtime polling on the right. `Ctrl-b z` zooms either
  pane. 2 actions, zero concept to learn.

## `tl` / `cs peek` rendering (Jobs target)

One line = one molecule. The emoji carries the kind. The state column
carries cognition progress. Plumbing (socket, PID, session name, git branch)
is accessible only via `cs peek <id>`.

```
🧠 delib-20260414-2ab2  evolve 3/5  panel dispatch      2m ago   hot
🔧 task-20260414-05db   done                            1h ago
💡 idea-20260413-9f1a   pending                         3h ago   warm
```

No `runtime-` prefix. No worker/runtime duplication. No socket column.

## Consequences

### Positive

- Operator mental model collapses to "one molecule = one line = one session".
- `cs patrol` becomes a one-liner (binary health signal per molecule).
- `cs purge` can safely clean up orphan runtimes and cognitions.
- Fleet composability (ADR-039) inherits the Gödel rule `Σ(m) := root_fleet_id(m)`
  for free.
- TLA+ / foundry spec (delib-e6b8) now has formalizable invariants (R1–R5,
  I1/I2/I8/I9) to bite on. Specifying the legacy architecture is not worth
  the effort — specifying Phase 3 is.

### Negative / risk

- Phase 3 touches `cosmon-transport`, `cosmon-cli/cmd/tackle.rs`,
  `cmd/run.rs`, `cmd/peek.rs`, `cmd/done.rs`. Estimated 500+ lines.
- `cs peek` needs pane-aware rendering (not just session-aware).
- `pane-died` hooks are tmux-version-dependent — MSRV audit needed.
- Old fleets in flight during the grace window require `cs reconcile`
  to recognize legacy two-session layouts and adopt them.

### Neutral

- Phase 1 is strictly additive: every existing deployment benefits from
  it immediately without opt-in.
- Phase 2 is opt-in only: no forced migration.

## Execution plan

Three child molecules, linked via `--blocked-by delib-20260414-2ab2`:

1. **task-040-phase1-role-field** — add `WorkerRole` to `fleet.json`, update
   `cs peek` / `cs purge` / `cs patrol` / `cs ensemble` to use it, add
   `cs reconcile` heuristic migration. Tests: serde roundtrip, migration
   from legacy file, fused-row rendering. `temp:warm`.

2. **task-040-phase2-socket-scheme-opt-in** — add `[transport] socket_scheme`
   parser, audit `cs peek --all` socket discovery regex, document the opt-in
   procedure. Tests: both schemes side-by-side, `--all` aggregation. Depends
   on Phase 1. `temp:warm`.

3. **task-040-phase3-split-window-session** — the structural refactor.
   Introduce pane-aware transport, rewrite `cs tackle` for orchestrating
   molecules, implement R1–R5 coupling invariants, add `cs patrol` binary
   health probe. Tests: crash injection (kill cognition pane, kill runtime
   pane), atomic teardown, `cs done` under each crash path. Depends on
   Phases 1 + 2. `temp:warm`.

Phase 1 is the critical path. Phase 2 is opt-in cosmetic. Phase 3 is the
durable fix and should not be rushed.

## Panel verdict summary (delib-20260414-2ab2)

9-persona panel. Condensed tally, with each persona's decisive angle:

| Persona   | Vote                | Angle                 | Decisive argument                                                                    |
|-----------|---------------------|-----------------------|--------------------------------------------------------------------------------------|
| wheeler   | Opt 4 (Opt 2 bridge)| session ≠ molecule    | Runtime is infrastructure, not cognition. Mis-projected as peer of the worker.       |
| einstein  | **Opt 3**           | invariants / parsimony| Adds 0 invariant. Opt 4 violates I7 (stateless). Opt 5 rewrites I10 (regime purity). |
| godel     | **Opt 3**           | recursive decidability| Only option satisfying (I)(S)(F)(A) at every N. `S(m) := molecule_id(m)`, no nesting.|
| torvalds  | Opt 2               | ship-minimum          | 30–80 lines, 1–2 days, zero migrator. Everything else is out of budget.              |
| tolnay    | Opt 2 opt-in        | semver                | `socket_scheme = "single" \| "split_by_role"`, default `"single"`. Minor bump.       |
| feynman   | **Opt 3**           | tenant_auditor test            | 2 actions, Claude visible immediately, zero concept to learn.                        |
| jobs      | orthogonal          | `tl` vocabulary       | One word = **molecule**. Forces merged rendering under every option.                 |
| hawking   | **Opt 3**           | boundaries + recovery | Bijection session ↔ mol. `pane-died → kill-session` atomic. Zero leaks. R1–R5.       |
| shannon   | 1-bit `role` field  | minimal information   | (1) ≡ (2) modulo string socket. Real gap is a semantic discriminant on workers.      |

**Tally** — Opt 3: 5 votes (einstein, godel, feynman, hawking + wheeler by
proxy via Opt 4 which points at the same topology). Opt 2: 2 (torvalds,
tolnay). Opt 1 / Opt 5: 0 votes, eliminated unanimously. Jobs and shannon
are orthogonal: their verdicts apply to every transport option, not one
of them — which is exactly why their insights became Phase 1 here.

**Convergence C1** — einstein, godel, hawking, feynman, jobs all reach the
same conclusion in different vocabularies: `1 molecule = 1 observable entity`.
The four observed bugs are symptoms of that broken bijection, not independent
UX issues. A geometric fix (Phase 3) makes them structurally impossible.

**Convergence C3** — shannon's explicit claim (and every persona's latent
assumption): the current schema cannot distinguish runtime from cognition
*anywhere on disk*. That is what makes Phase 1 the critical path: the
smallest change with the biggest leverage. `cs peek` dedupe, `cs purge`
safety, and `cs patrol` detection all collapse to one bit.

Full synthesis (all persona votes, D1–D4 divergences, I1–I5 insights):
`.cosmon/state/fleets/default/molecules/delib-20260414-2ab2/synthesis.md`.

## Interaction with sibling deliberations

- **delib-20260414-778e — fleet composability.** Option 3 is the only transport
  that stays decidable when fleets nest. The Gödel rule `Σ(m) := root_fleet_id(m)`
  (constant along any composition tree) is the invariant that lets a master
  fleet reference children by `mol_id` instead of by session — no joint state
  machine across levels, no fresh socket per nesting depth. Phases 1 and 2 are
  neutral w.r.t. composability; Phase 3 *enables* it by making recursive fleet
  dispatch structurally trivial (one socket per project, one session per mol,
  independent trees).

- **delib-20260414-e6b8 — formal spec (TLA+ / foundry).** Re-architect first,
  specify second. Specifying the legacy two-session geometry is not worth the
  effort — it is already known to be buggy, and R1–R5 / I1–I9 would be awkward
  to encode over it. Specifying Phase 3 is worth the effort: the invariants
  above are already in TLA+-digestible form. Recommendation: delib-e6b8 waits
  for Phase 3 to land before biting on real architecture.

## Open questions (for a successor ADR or Phase 3 RFC)

1. **Migration protocol for in-flight legacy fleets.** Can `cs reconcile`
   detect a pre-ADR-040 two-session layout and adopt it, or must the operator
   drain legacy fleets before upgrading? Default: adopt; fall back to drain
   with a clear message if pane topology is ambiguous.
2. **Pane-died hook portability.** Which tmux versions expose the hook with
   the semantics we need? What is the MSRV for the macOS and Linux tarballs
   we ship with? Phase 3 cannot ship without an answer.
3. **Runtime daemon (Option 4) as successor.** When (and if) ADR-016 Phase 3
   introduces a resident runtime, does split-window become redundant or does
   it remain the default human-observability story? Likely the latter — the
   resident runtime owns L3 policy, not per-molecule cognition observability.
4. **Cross-galaxy edges (ADR-035).** Does the bijection survive when a
   molecule in project A blocks a molecule in project B? Σ(m) changes at the
   boundary. Phase 3 needs an explicit rule for cross-socket references.
