# ADR-095 — Resident Runtime: ratification of the build path under the IFBDD lens

**Status:** Accepted (2026-05-17).
**Date:** 2026-05-17.
**Scope:** Reverses the empirical-retirement of ADR-016 Phases 3+ pronounced
by ADR-054. Re-opens the build path
for the Resident Runtime as a *constrained client* of the transactional core,
under five named structural invariants and an explicit forensics-first
(IFBDD) construction order. Preserves ADR-054's load-bearing inheritances
(three-regime vocabulary, Markov property, one-layer-of-truth framing,
`cs harvest` as the event-driven cure for the worker-exit pathology).
**Date of supersession of ADR-054 (partial):** 2026-05-17.

**Parent deliberation:**
`delib-20260517-1374`
— seven-persona panel on `adopt(OpenClaw) / build / refuse`. Verdict tally
on its face: `adopt × 0` / `build × 3` / `refuse × 4`. Synthesis convergence
C4 (karpathy's badge — *"you can `cat` cosmon's state"*) and the
`build`-camp / `refuse`-camp tension framed as a *sequencing* dispute
(synthesis "The real tension — `build` vs `refuse`"). The build-camp
panelists (torvalds, forgemaster, karpathy) and the refuse-camp
architect each describe the *same* structural artifact: a thin loop,
client of the transactional core, no new state, deletable as a single
Cargo target.

**Operator overrule, named for the record.** The 4-3 majority is *refuse*
on the panel scoreboard. The operator's verdict — read out at the
synthesis post-mortem — is **`build`**, justified by the IFBDD lens and
two structural facts the panel deliberation did not weigh:
(i) cosmon's sole-operator regime today, and
(ii) the predictable arrival of decentralized noyau-instances that
cannot share a host clock. The verdict is *not* a rejection of the
refuse camp's structural arguments — they are absorbed verbatim as
the five named invariants in §3. It is a rejection of the refuse camp's
*sequencing* argument: that the loop's invariants should be encoded
in convention until forensic evidence materialises. Under IFBDD, the
forensic instrument must be built *before* the behaviour it instruments;
the refuse camp's sequencing inverts that order.

**Supersedes (partial):** ADR-054
— specifically §1 (retire ADR-016 Phase 3+) and §4 (fate of `cs run` —
"dies"). The vocabulary survival in §3, the Markov-property preservation
in §"Invariants — Preserved", and the redefinition of *Autonomous* as
tenant-owned are inherited unchanged.
**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — Phase 3+
  un-retired by this ADR, with the design constraints in §3 below.
- [ADR-022](022-native-dag-scheduler.md) — the native DAG scheduler
  remains the planner substrate of choice; the Resident Runtime is its
  in-process consumer.
- `docs/architectural-invariants.md` §14 (karpathy's load-bearing
  invariant — *"you can `cat` cosmon's state"*) — adopted as the
  bedrock test every Resident Runtime PR must pass.

## Context

ADR-054 (2026-04-20) retired
ADR-016 Phases 3+ on two grounds:

1. **Empirical absence.** Eleven months after ADR-016 was proposed,
   Phase 3 sat at the skeleton level (`DagPolicy` + `NoOpPolicy`,
   `cs run` as subprocess driver); Phases 4–5 unbuilt. *"No concrete
   use case has materialized."*
2. **Externalisable substitute.** The pathology Phase 3 was meant to
   close (worker completion not propagating to `cs done`) was closed
   instead by `cs harvest` + the tmux `pane-died` hook, an
   event-driven cure that approaches Carnot's polling bound. The
   `cs tick` verb (syzygie panel `delib-20260418-1f29` C2, unanimous
   GO) gave operators the documented external clock.

ADR-054 was correct *under the empirical-feature-pull lens*. Twenty-eight
days later, a re-deliberation
(`delib-20260517-1374`)
re-opened the substrate question because a third party (NVIDIA OpenClaw
/ NemoClaw) was pitching its agent runtime as the natural cosmon
substrate. The panel unanimously rejected `adopt(host)` — seven of seven
arrive from seven different lenses at the same conclusion (synthesis
Convergence 1). But the deliberation surfaced a deeper question the
ADR-054 retirement did not address: **what is the right basis for the
build-or-not decision?**

### The IFBDD lens

IFBDD — *Investigation/Forensics-Before-Decision-Driven Development* —
is cosmon's vocabulary term for the rule that "observability before
feature" is itself the load-bearing decision (`docs/vocabulary.md`
§*Forensics*, item 4). The trace must exist before the behaviour it
traces is built; *"you must be able to investigate before you can
trust"*. Under this lens:

- The absence of a use case in eleven months is *evidence about the
  absence of a forensic instrument*, not evidence about the absence of
  latent demand. The skeleton `cosmon-runtime` crate shipped without
  the failure-mode hooks (no observability of stalls, races, ghost
  merges, lost-update on `.cosmon/state/`); the operator could not
  *see* a use case because the trace-capturing machinery was not in
  place.
- The forensic principle (`docs/vocabulary.md` §*Forensics — Operational
  Rule*) forbids relying on the agent's claim that "no use case
  surfaced". The trace would have been the events.jsonl tap galileo
  proposed in the deliberation
  (`delib-20260517-1374`);
  that trace does not exist for the eleven-month window ADR-054 cites.
- IFBDD therefore inverts the polarity of ADR-054's empirical argument.
  The honest IFBDD-grade statement is: *"We do not yet know whether a
  use case exists, because we never built the instrument to observe
  one."* That is a `build the instrument` verdict, not a
  `retire the path` verdict.

### The decentralized-instance constraint

A structural fact absent from ADR-054's frame: cosmon is currently a
**sole-operator** tool. Every external clock named in ADR-054 §4 (cron,
launchd, `cs tick` invoked from a human shell) assumes a *single host*
where the operator's own machine doubles as the dispatch surface.

The cosmon roadmap names *Noyau* — a community of trusted humans, each
carrying their own pilot-cognition (ADR-061, ADR-063). When a Noyau
instance runs on a peer's machine, *with no operator at the keyboard
in the LAN time-zone*, the external-clock substitute degrades: cron is
that host's cron, not the federation's; LaunchAgent is local; `cs tick`
requires *someone* to invoke it on that host. The decentralized
instance needs **local autonomy** — a process that holds the right
to advance its own propelled DAG without an external operator nudge.

ADR-054 implicitly assumed the sole-operator regime would remain
indefinitely. The IFBDD-grade move is to build the runtime that the
decentralized instance will require, *with the observability hooks
baked in from day one*, so that the failure-mode evidence ADR-054
wished for is *generated by the build itself*, not retroactively
extracted from a skeleton that never had the hooks.

### What the panel actually converged on

Synthesis "Convergence — the real tension" is decisive: the build-camp
artifact (torvalds' "client of the transactional core, no new schema,
deletable Cargo target"; forgemaster's "<500 LOC `cosmon-runtime` crate,
DagPolicy + RuntimeLoop, deletable without touching cosmon-core";
karpathy's "200-line `cs run --resident` spike under launchd/systemd")
is **structurally identical** to the refuse-camp's hexagonal sketch
(architect's "smallest residential runtime that isn't a daemon-in-disguise"
converges on the same skeleton). The panel did not split on *what to
build* — it split on *whether to name it*.

This ADR names it.

## Decision

### 1. Ratify the build path — Resident Runtime as a constrained client of the transactional core

The Resident Runtime work foreseen by ADR-016 Phase 3+ is **re-opened**
and ratified, under the structural invariants in §2 below. The
`cosmon-runtime` crate skeleton retained in-tree at ADR-054 acceptance
(scheduled for deletion under ADR-054 §4 deletion criteria) is the seed
of the build. ADR-054 §4 deletion criteria are **retracted** — the
deletion is no longer scheduled, conditional or otherwise. The crate is
now a load-bearing target with five named invariants and a forensics-
first construction order.

`cs run` is **re-instated** as a long-running scheduler verb. ADR-054
§4's "dies" verdict on `cs run` is reversed. The migration path
described in ADR-054 §4.2 (replace `cs run` with `cs tick` + LaunchAgent)
remains *one* valid composition — operators who built that pattern keep
it. The Resident Runtime is the *additive* path that did not exist
before.

### 2. Five named structural invariants the build MUST preserve

The Resident Runtime is admitted into the architecture if and only if
all five invariants hold by construction. Any PR that weakens any one
of them is a structural breach (file a bead; do not patch the
adapter).

**Invariant RR-1 — Client of the transactional core; never substrate-beneath.**
The Resident Runtime is a *client* of the `cs` CLI. It calls
`cs tackle`, `cs evolve`, `cs complete`, `cs done`, `cs harvest`,
`cs reconcile` from its own loop the same way a human would from a
shell. It never reaches into `cosmon-core` mutators directly. It never
imports `cosmon-state`, `cosmon-filestore`, or any other state-mutation
crate. The transactional core's CLI surface is the *only* mutation path.
Test: `cargo tree -p cosmon-runtime --no-default-features` shows zero
edges to state-mutating crates; CI enforces. (Mirrors ADR-080 §8p's
"API surface ⊊ CLI surface" pattern.)

**Invariant RR-2 — Owns no state.**
The Resident Runtime reads and writes only the existing
`.cosmon/state/` JSON files via the CLI verbs of RR-1. It introduces
no new schema, no new state file, no new directory. Its own in-memory
caches are advisory only — every decision is re-derivable by replaying
`events.jsonl` (cf. §8d *events.jsonl is source-of-truth*). The runtime
holds the right to perform the next observation; it does not hold the
state being observed.

**Invariant RR-3 — Deletable as a single Cargo target without touching `cosmon-core`.**
The `cosmon-runtime` crate (and the `cs run` command body in
`cosmon-cli`) form a closed Cargo unit. Removing the crate from the
workspace, removing the `cs run` subcommand from `cosmon-cli`, and
running `cargo check --workspace` must leave the rest of the tree
green. This is the structural escape hatch: if RR fails the forensic
evidence test (§4 below), it can be excised with one PR. Test:
`scripts/runtime-excision-test.sh` (to be written under
`task-20260517-rr-excision`) deletes the crate, the binary subcommand,
and the workspace member, and asserts `cargo check --workspace` is
green; runs as a periodic CI job.

**Invariant RR-4 — JSON-on-disk remains the authoritative source of truth.**
The Resident Runtime never becomes the source of truth. A human running
`cs observe`, `cs ensemble`, `cs peek`, `cs reconcile` from any other
shell, or another process running concurrent `cs` commands, sees the
same state the runtime sees. There is no runtime-resident "true" view
that the on-disk view shadows. If the runtime crashes, restarting it
from the disk state produces the same continuation. Test:
*kill -9* the runtime mid-dispatch, restart it, verify the next
dispatched molecule is the one the killed runtime would have chosen.
(This is the Markov-property restart-fidelity test from §7c, applied
to the runtime.)

**Invariant RR-5 — Failure-mode observability hooks baked in from day one (NEW; the operator's IFBDD requirement).**
Every event the Resident Runtime *could* fail to emit forensic
evidence on is wired into `events.jsonl` *before* the corresponding
behaviour ships. The taxonomy is the four silent-failure modes
forgemaster named in
`delib-20260517-1374/responses/forgemaster.md`:
- **TOCTOU on `.cosmon/state/`** — every read-decide-write triple
  emits a `RuntimeReadDecideWrite` event with the file's pre-read
  mtime and post-write mtime, so a future audit can detect the
  read-from-stale-write pattern.
- **Double `cs evolve`** — every CLI-shell-out emits a
  `RuntimeShelledOut` event keyed by `(mol_id, step_n, invocation_uuid)`;
  a duplicate `(mol_id, step_n)` with distinct `invocation_uuid` is a
  detectable idempotency violation.
- **Ghost merge bypass** — every `cs done` shell-out by the runtime
  emits a `RuntimeMergeDispatched` event *before* the call; a `cs done`
  side-effect with no matching `RuntimeMergeDispatched` upstream means
  the runtime bypassed its own dispatch path.
- **Stolen worktrees** — every `cs tackle` shell-out emits a
  `RuntimeWorktreeClaimed` event with the worktree path; concurrent
  claims on the same path are a detectable race.

The four event variants ship in the **same PR** as the loop they
instrument. A PR that adds a behaviour without its observability
hook fails CI. This is the load-bearing IFBDD discipline: the
instrument exists before the behaviour. (Mirrors §8b
*briefing seals* — soft contract, not a lock; the seal catches the
lazy shadow contract, not a motivated adversary.)

### 3. Relationship to ADR-054 — explicit handling

ADR-054 has four sections of normative content. This ADR's relationship
to each:

| ADR-054 section | This ADR's verdict | Rationale |
|-----------------|--------------------|-----------|
| §1 *Retire ADR-016 Phase 3+ entirely* | **Reversed.** Phase 3+ is re-opened under the §2 invariants. | The empirical-retirement basis (eleven months, no use case) is invalidated by IFBDD: the absence of a forensic instrument is not evidence of the absence of demand. |
| §2 *Future planner work, if needed, gets a fresh ADR* | **Inherited verbatim.** This ADR *is* that fresh ADR. | ADR-054 §2 forbade a partial revival of ADR-016; this ADR is a *full* ratification under new invariants, not a partial revival. |
| §3 *Three-regime vocabulary survives, with redefinition* | **Inherited verbatim** (Autonomous = tenant-owned). | Wheeler's framing — *liveness is the delegation of the right to observe* — survives intact; the Resident Runtime is one valid tenant, not the only one. claude-code via MCP remains a co-tenant. |
| §4 *`cs run` dies* | **Reversed.** `cs run` is re-instated as a long-running scheduler verb. | Deletion criteria retracted. `cs harvest` + the tmux `pane-died` hook remain the event-driven cure for the worker-exit pathology; `cs run` is the *additive* path for the sole-operator and decentralized-instance regimes that need local autonomy. |
| §5 *Two-layer diagram collapses to one layer* | **Reversed.** Two layers restored. | But the Resident Runtime layer is a *constrained client* per §2 (RR-1 through RR-5), not the substrate ADR-016 originally drew. The one-layer simplification ADR-054 introduced is sacrificed for the IFBDD-grade build; the architectural-invariants doc §1 is updated in lockstep. |

ADR-054 is not retracted — it remains the historical record of the
correct conclusion under the empirical-feature-pull lens. This ADR is
the supersession under the IFBDD lens, and the supersession is partial:
ADR-054 §2 and §3 survive.

### 4. Falsification criteria (galileo's discipline, adapted to the build path)

Borrowed verbatim from
`delib-20260517-1374/responses/galileo.md`
(with the polarity flipped — galileo's instrument was a *passive tap*
to settle build-vs-refuse; here the instrument is the live runtime
itself, and the falsification target is the build).

The build path is **falsified** if, **90 days** after the first PR
that wires RR-1 through RR-5 into a green CI:

- **≥1 silent-drift incident** detected on the four
  silent-failure-mode hooks (RR-5) that was not caught by the operator
  within the next 24h — *and* the same incident class is
  reproducible across two distinct invocations. The forensic
  instrument is what makes this falsifiable; it is the IFBDD pact.
- **Median operator-postman load (the gap between `cs done` of a
  blocker and `cs tackle` of its dependent)** has not dropped by
  ≥50% in the 90-day window. (galileo's "build path falsified by no
  ≥50% drop in operator gaps after 90 days", inherited.)
- **Zero new use cases** materialise that the external-clock
  composition (cron / LaunchAgent / `cs tick`) could not equivalently
  serve. (galileo's "AGAINST signals three-of-four" criterion, adapted:
  here the threshold is *zero new use cases*, because if the IFBDD
  instrument is in place and *still* fails to surface latent demand,
  the operator's hypothesis about decentralized instances was wrong.)

If any one of the three triggers fires, the Resident Runtime is
excised via RR-3 (the deletion is one PR by construction) and a
**fresh ADR** ratifies the retirement with the forensic evidence that
ADR-054 lacked. The deletion is not a defeat; it is the IFBDD pact
honoured.

### 5. Install ritual — via `cosmon-daemon-supervisor`, not a standalone LaunchAgent

The Resident Runtime is a long-running cosmon-orbit process. Every
other long-running cosmon-orbit process on the operator's machine
(notification-bot, incredibles-bot, notification-bot, emacs-daemon,
almanac, archive-service, …) is declared in `~/.config/cosmon/daemons.toml`
and supervised by `cosmon-daemon-supervisor`
([ADR-053](053-cosmon-daemon-supervisor.md)). The Resident Runtime
**MUST** use the same canal.

**Canonical install canal — bind:**

The operator installs the Resident Runtime by **adding a `[[daemon]]`
block** to `~/.config/cosmon/daemons.toml`, with `enabled = false`
as the default. The supervisor hot-reloads on save (`notify`
debounce ≈ 200 ms); flipping `enabled = true` then `:w` is the
*activate* gesture. Flipping it back to `false` (or `touch`-ing the
per-daemon `kill_switch`) is the *deactivate* gesture. No
`launchctl`, no `sed`, no per-daemon plist. The canonical template
the operator pastes lives at
[`docs/guides/install-resident-runtime.md`](../guides/install-resident-runtime.md).

**Why this canal and not a standalone LaunchAgent:**

- **Namespace coherence.** cosmon-orbit LaunchAgents are `com.cosmon.*`
  (scheduler, daemon-supervisor). A `com.noogram.*` agent would have
  been a fourth namespace on the same machine, hostile to operator
  memory.
- **One concept, infinite extensibility** (CLAUDE.md §Composability,
  ADR-053 §11). Adding a daemon is one TOML entry; the operator
  acquires no new ritual.
- **Throttle / kill-switch / log discipline already exists.** The
  supervisor's `throttle_seconds`, per-daemon `kill_switch`, and
  global `stand-down.lock` give the IFBDD pact its enforcement
  surface for free. The previous standalone template had to
  re-invent these (it set `KeepAlive = false` to surface crashes; the
  supervisor's per-daemon `enabled = false` is the same gesture with
  fewer mechanisms).
- **Retraction symmetry.** Excision under RR-3 means deleting the
  TOML block, not running `launchctl bootout`. The decision-reversal
  cost matches the decision-installation cost: one file edit.

**IFBDD opt-in pact — preserved by construction:**

The supervisor only spawns a child when `enabled = true`. The block
ships in the install guide with `enabled = false`, so the runtime
sits dormant until the operator commits the flip. The 90-day
forensic gate (§4) starts ticking from the first `enabled = true`
edit on the operator's machine, not from this ADR's acceptance. The
`events.jsonl` instrument (RR-5) is in place before either edit.

**Historical witness.** The deprecated standalone template lives at
`scripts/launchd/archived/com.noogram.cosmon.runtime.plist.deprecated`
with a header explaining why it was retired. It is documentation,
not an installable artifact. (Origin: this ADR's first build wave
shipped the standalone template by reflex; `task-20260518-b420`
realigned it with ADR-053.)

**Self-hosting loop — orthogonal.** The `just self-runtime` target
(foreground, `tail -F`, Ctrl-C to stop) remains the developer-side
gesture for observing live dispatch decisions. It is not an
install; it is the inverse — a temporary, attached runtime for
investigation. The supervisor-managed daemon is the production
canal; `self-runtime` is the workshop.

## Options Considered

### Option A — Keep ADR-054 as-is; defer the build until empirical demand materialises (rejected)

The conservative read of the deliberation: 4-3 majority refuses;
honour the panel scoreboard; nucleate galileo's passive tap; revisit
in 60 days.

- **Pros:** preserves the one-layer simplification ADR-054 introduced;
  minimum cognitive load on the operator today; the build-camp's
  five invariants can be encoded in the next ADR if the evidence
  arrives.
- **Cons (decisive):** the IFBDD lens forbids this sequencing. The
  forensic instrument galileo proposed is a *passive observability
  tap* on `events.jsonl` — it can see *what already happens*, but it
  cannot see *what would happen if a runtime existed* (the
  decentralized-instance use case is not generable by an external
  clock; the use case is conditional on the runtime existing). The
  passive tap is insufficient for the question.
- **Why rejected:** waiting for empirical demand from a regime that
  *cannot exist without the runtime* is a category error. The IFBDD
  pact is to build the instrument *before* the behaviour, not after.

### Option B — Adopt OpenClaw/NemoClaw as substrate (foreclosed by panel)

Synthesis Convergence 1 (`adopt(host)` × 0) records seven-of-seven
rejection from seven independent lenses. Not reconsidered here.

### Option C — Ratify the build path under the IFBDD lens, with five named structural invariants (accepted, this ADR)

Names the artifact the build-camp and the refuse-camp's hexagonal
sketch both converge on, with explicit construction order (forensic
hooks first, behaviour second) and explicit excision path (RR-3) so
the decision is reversible at low cost if falsified (§4).

- **Pros:** unblocks the decentralized-instance regime; pays the
  IFBDD pact in full; absorbs the refuse-camp's structural arguments
  verbatim as the five invariants; preserves the excision path; gives
  the operator the load-bearing observability hooks before the
  behaviour ships.
- **Cons:** re-introduces the two-layer cognitive load ADR-054
  collapsed; commits to a 90-day forensic measurement window; the
  build can fail (§4) and the path back to the one-layer architecture
  is a deletion PR plus a fresh ADR. The fresh-ADR-on-retirement
  burden is part of the IFBDD pact.

### Option D — Jobs' alternative: rename "Resident Runtime" → "long-form `cs run` mode" everywhere; refuse the build (rejected, named for the record)

Jobs' verdict from the deliberation
(`responses/jobs.md`):
the thin loop already has a name (`cs run`); naming it twice doctrinalises
a position ADR-054 already retired.

- **Pros:** maximum subtractive design; preserves jobs' "no daemon"
  rule unconditionally; preserves the wedge.
- **Cons:** jobs' rule against named exceptions cuts both ways. The
  exception is being made *because* the IFBDD discipline forbids
  building the behaviour without the instrument, and the existing
  `cs run` shipped without the instrument. Renaming the existing
  un-instrumented `cs run` to "long-form mode" does not pay the IFBDD
  pact; it doctrinalises the un-instrumented version.
- **Why rejected (named for the record per ADR-082 INV-ADR-OPTIONS-CONSIDERED):**
  Jobs' argument is decisive *under the empirical-feature-pull lens*
  (the lens that produced ADR-054). Under the IFBDD lens it is
  invertible: the right response to "the loop ships without an
  instrument" is to ship the instrument, not to ratify the
  instrument-less loop. Jobs' subtractive verdict is honoured at
  the level of the *artifact* (the build remains a thin loop,
  ≤500 LOC, deletable target) but not at the level of the *naming*
  (the loop is named, instrumented, and ratified).

## Consequences

**Positive.**

- **The IFBDD pact is honoured.** The forensic instrument exists
  before the behaviour. Every silent-failure mode forgemaster named
  is wired into `events.jsonl` *before* the corresponding code path
  ships. A future audit can answer the question ADR-054 wished it
  could answer: *"did a use case ever surface?"* — with trace data,
  not with the agent's claim.
- **The decentralized-instance regime is unblocked.** Future Noyau
  peers running on their own hosts gain a local clock that does not
  require an external operator. The sole-operator assumption ADR-054
  carried silently is named and overridden.
- **The refuse-camp's structural arguments are absorbed verbatim.**
  RR-1 through RR-4 are the architect / torvalds / forgemaster /
  karpathy invariants the deliberation converged on; RR-5 is the
  operator's IFBDD requirement on top. The build is not a rejection
  of the refuse camp; it is the build the refuse camp would have
  designed if it had agreed to name the loop.
- **The excision path is named.** RR-3 ensures the runtime is
  deletable as one PR. The decision is *reversible*; ADR-054 §4's
  one-way retirement is replaced with a two-way gate (build *and*
  excise paths both pre-specified).

**Negative / accepted.**

- **The one-layer simplification ADR-054 introduced is reverted.**
  Two layers are restored, with the cognitive load that implies. The
  trade is: cognitive load now, in exchange for forensic evidence
  ADR-054 lacked.
- **The 90-day forensic measurement window is a commitment.** The
  operator commits to surfacing the events.jsonl evidence at the
  90-day mark, regardless of whether the verdict is build-confirmed
  or build-falsified. The IFBDD pact requires this; it is not
  optional.
- **`cs run` is doctrinally re-named.** Operators who migrated to
  `cs tick` + LaunchAgent under ADR-054 §4.2 keep that path; `cs run`
  is the *additive* path for the regimes that need local autonomy.
  Documentation must clarify which path is right for which regime.
- **Jobs' "no named exceptions" rule is bent.** The Resident Runtime
  is a named exception to the "no daemon" rule. The exception is
  bounded by RR-1 through RR-5 and the §4 falsification criteria; the
  exception is *not* the door for further daemons.

**Structural.**

- ADR-054 is marked **Superseded in part by ADR-095**; ADR-054 §1
  and §4 are reversed, §2 and §3 are inherited.
- ADR-016 Phase 3+ is **un-retired**; the Phase 3+ section in ADR-016
  is updated with a callout pointing to this ADR and to
  `docs/architectural-invariants.md` §14 (karpathy's invariant).
- `docs/architectural-invariants.md` §1 is restored to the two-layer
  diagram with the §2 invariants from this ADR cited as the
  constraint set for the Resident Runtime layer.
- `docs/architectural-invariants.md` §14 (new) inscribes karpathy's
  load-bearing tatouage *"you can `cat` cosmon's state"*.
- A follow-up `task-20260517-rr-excision` writes
  `scripts/runtime-excision-test.sh` so RR-3 is a CI-enforced
  invariant from the first PR of the build.

## Invariants

**Preserved (inherited from ADR-054 §"Invariants — Preserved").**

- **Markov property** (`docs/architectural-invariants.md` §7c). The
  Resident Runtime is a *pure function of disk state*; RR-2 and RR-4
  enforce this by construction.
- **Stateless CLI.** Every `cs` command remains one-shot. The Resident
  Runtime is a long-running *client* of stateless commands; the
  commands themselves do not gain a daemon flavor.
- **Three regimes describe observation delegation, not runtime
  topology.** Wheeler's framing survives intact (ADR-054 §3,
  inherited). The Resident Runtime is *one* tenant in the Autonomous
  regime; claude-code via MCP is another; a future planner crate
  would be a third.
- **Worker/human boundary** (`docs/architectural-invariants.md` §3
  *Two boundaries*). The Resident Runtime calls `cs done`,
  `cs harvest`, `cs tackle` from a sibling shell — never inside a
  worker's worktree.

**Added (this ADR).**

- **RR-1 through RR-5** (§2 above). Five named structural invariants
  on the `cosmon-runtime` crate. Any PR that weakens any one of them
  is a structural breach.
- **Karpathy's badge** (`docs/architectural-invariants.md` §14, new).
  *You can `cat` cosmon's state.* Every Resident Runtime PR must
  pass this test: after the PR, a peer running `cat
  .cosmon/state/fleets/default/molecules/<mol_id>/state.json` in any
  other shell sees the same state the runtime sees.

**Modified.**

- **Two-layer architecture** (`docs/architectural-invariants.md` §1).
  Restored from the one-layer collapse ADR-054 §5 introduced. The
  Resident Runtime layer is a *constrained client* per §2, not the
  substrate ADR-016 originally drew.

**Re-instated.**

- **`cs run` as long-running scheduler verb.** ADR-054 §4's "dies"
  verdict is reversed. Migration paths to `cs tick` + LaunchAgent
  remain valid for operators who adopted them; `cs run` is the
  additive path for the sole-operator and decentralized-instance
  regimes.

## Implementation sequence

This ADR is documentation-only at acceptance. Code follows on the
IFBDD construction order: instrument first, behaviour second.

1. **Immediate (this ADR):**
   - Mark ADR-054 §1 and §4 as Superseded by ADR-095.
   - Update ADR-016 Phase 3+ section with the un-retirement callout
     pointing to this ADR and to §14.
   - Add `docs/architectural-invariants.md` §14 (karpathy's badge).
   - Restore `docs/architectural-invariants.md` §1 two-layer diagram
     with RR-1 through RR-5 cited as the constraint set.
   - Add CHANGELOG entry; `cs reconcile` updates `docs/adr/INDEX.md`.

2. **Build phase 1 — forensic hooks (the IFBDD instrument):**
   - Add the four `EventV2` variants from RR-5
     (`RuntimeReadDecideWrite`, `RuntimeShelledOut`,
     `RuntimeMergeDispatched`, `RuntimeWorktreeClaimed`) to
     `cosmon-core`, with serde round-trip tests.
   - Write `scripts/runtime-excision-test.sh` (RR-3).
   - This PR ships *before* any runtime loop code. The test gates
     CI on the four variants existing and the excision script
     passing on an empty `cosmon-runtime`.

3. **Build phase 2 — the loop:**
   - `cosmon-runtime` crate: `DagPolicy` trait, `RuntimeLoop` struct,
     `cs run --dag <root>` consumer. Every behaviour emits the
     corresponding RR-5 event *before* the side-effect.
   - Markov restart-fidelity test (RR-4): kill -9 mid-dispatch,
     restart, verify next dispatch is identical.

4. **Build phase 3 — decentralized instance:**
   - The first Noyau peer's runtime instance runs against the same
     `.cosmon/state/` JSON files (RR-4); the events.jsonl trace is
     the federation-wide forensic record.

5. **90-day forensic evaluation gate (§4):**
   - At day 90 from phase-2 ship, the operator surfaces the
     events.jsonl evidence. Verdict: build-confirmed, build-extended,
     or build-falsified. Build-falsified path is RR-3 excision + a
     fresh ADR ratifying the retirement with forensic evidence.

## References

- **Parent deliberation:**
  `delib-20260517-1374/synthesis.md`
  — seven-persona panel; per-persona responses under
  `responses/`. Build camp: torvalds, forgemaster, karpathy. Refuse
  camp: architect, niel, jobs, galileo.
- **Partially superseded:**
  ADR-054 §1, §4 (reversed);
  §2, §3 (inherited).
- **Un-retired:**
  [ADR-016](016-autonomy-regimes-and-resident-runtime.md) Phases 3–5
  (with the design constraints in §2 of this ADR).
- **Bound:**
  [ADR-022](022-native-dag-scheduler.md) — native DAG scheduler;
  the Resident Runtime is its in-process consumer.
- **Vocabulary:** `docs/vocabulary.md` §*Forensics* (the IFBDD lens
  definition), `docs/architectural-invariants.md` §14 (karpathy's
  badge, new), §7c (Markov property), §3 *Two boundaries* (worker/
  not-worker), §8b (briefing seals — soft contract precedent).
- **Decentralized-instance roadmap:**
  [ADR-061](061-pilot-session-and-causal-closure.md) (Nucléon /
  Orbitale / Noyau / Phase), [ADR-063](063-vocabulary-orbitale-nucleon-noyau-phase.md)
  (vocabulary), `CLAUDE.md` *Atomic-nucleon family*.
- **Operator overrule precedent:** ADR-016's
  *"if the code contradicts either thesis or the invariants, the
  thesis wins"* clause is the structural license for this overrule;
  the IFBDD lens is the doctrinal warrant.
