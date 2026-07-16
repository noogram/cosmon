# ADR-049: Cosmon-ward Feedback Flow

**Status:** Proposed
**Date:** 2026-04-17
**Inaugural chronicle:** an internal chronicle
§*2026-04-17 nuit — Le réacteur apprend de ce qu'il brûle*
**Related:** [ADR-048](048-backlog-sanity-invariant.md) (first instance —
convoy cascade signal turned into binding decision)

## Context

Cosmon is not neutral infrastructure. It is the **reactor core** that
orchestrates a fleet of *application-site galaxies*: mailroom,
showroom, sandbox, wiki2-audit, and others to come.
Each of those galaxies *pressure-tests* cosmon's primitives every time
a pilot drives them. When a primitive cracks under that pressure, the
crack appears in the application — but the crack is in cosmon.

The asymmetry is honest and worth naming explicitly:

- **Cosmon ↔ application-site is not symmetric.** Cosmon serves the
  galaxies; the galaxies stress-test cosmon in return. This is the
  contrapuntal exchange the *Le réacteur apprend de ce qu'il brûle*
  chronicle describes: *« le carburant enseigne au réacteur ce dont il
  a besoin »*.
- **The closest surface to the incident is the application; the
  surface that must learn is cosmon.** A pilot working on mailroom
  who hits a cosmon pathology has a tempting cheap fix in the
  application (a wrapper, a workaround, a `# TODO`). That fix is
  *silent erosion*: the symptom disappears, the underlying defect
  stays, and cosmon never gets the signal that one of its
  primitives is broken.
- **The rule is already inscribed in two places** but not yet in a
  citable architectural artifact:
  - The chronicle entry above (long-form rationale, narrative).
  - `~/.claude/CLAUDE.md` Core Rules (cross-session pilot discipline).

This ADR promotes the rule to a binding cosmon decision so future
chronicles in *any* galaxy can cite it by stable relative path
(`docs/adr/049-cosmon-ward-feedback-flow.md`) rather than by quoting
prose from a global config or a chronicle paragraph.

### Two inaugural examples

The principle is not abstract. Two cases from the same evening
(2026-04-17) show its shape:

1. **Convoy cascade → ADR-048.** While tackling DAG roots `78bf` and
   `87cd` on mailroom, `cs tackle` auto-upgraded to runtime-mode and
   resurrected 14 pending molecules from 2026-04-14, burning ~40 min of
   worker cycles on zombie work. The pilot's *cheap* fix would have
   been: add `--leaf` to the pilot's mental model and move on. The
   *cosmon-ward* fix was: nucleate `idea-20260417-ba1d`, evaluate three
   reaction modes, draft [ADR-048](048-backlog-sanity-invariant.md)
   (backlog-sanity invariant of the Autonomous regime), and a sibling
   implementation task. The pathology is now mechanically prevented by
   a typed `GuardError`, not by pilot folklore.
2. **Spawn-postcondition timeout (N = 1) → observation chronicle.**
   `cs tackle` failed once with *"spawn postcondition failed — session
   never produced live-claude output within 2s"*; an immediate retry
   succeeded. Analysis (`task-20260417-a588`) showed a single 2 s timer
   enforces two physically distinct tests (exec survival, ~100 ms;
   claude-print recognition, dominated by cache state and unbounded).
   The cheap fix would have been: bump the timeout to 5 s. The
   cosmon-ward response was: write the analysis, publish
   an internal chronicle,
   and adopt an explicit decision rule (*N=1 → wait; second occurrence
   in 60 d → refactor as task-work*). No silent timeout bump, no
   premature ADR — the molecule itself encodes the discipline.

The first example shows cosmon-ward when the signal is strong enough
to warrant an immediate ADR; the second shows it when N = 1 and the
honest answer is *observe and wait*. Both are valid cosmon-ward
responses; both are explicitly *not silent patches*.

## Decision

### 1. The rule

When a pilot working in an application-site galaxy (mailroom,
showroom, sandbox, wiki2-audit, …) encounters a
**broken cosmon invariant** or a **missing cosmon primitive**, the
pilot **MUST** surface it back to cosmon as a typed molecule:

- `cs nucleate ... --kind issue` — observed pathology, not yet a
  decision.
- `cs nucleate ... --kind task` — concrete remediation work.
- `cs nucleate ... --kind idea` — when the response space is open
  and needs an `idea-to-plan` cycle (capture → evaluate → plan).
- `cs nucleate ... --kind decision` (i.e. an ADR) — when the
  remediation is structural and warrants a binding architectural
  artifact.

Silent-patching in the application — wrappers, workarounds, ad-hoc
TODOs that absorb the cosmon defect locally — is **forbidden**. This
is the *silent-ignore is forbidden* rule of cross-galaxy etiquette
(see ADR on cross-galaxy edges and the operator-discipline corpus),
applied in the cosmon-ward direction.

### 2. The qualifier — not every friction is cosmon-ward

The rule is binding but bounded. The test is:

> *Is a cosmon invariant broken or is a cosmon primitive missing?*

If yes → cosmon-ward. If the answer is merely *"something rubbed,
something was awkward, something took longer than expected"* → not
cosmon-ward. Friction in pilot ergonomics, in galaxy-local
conventions, or in user-level tooling is handled inside the galaxy.

Distinguishing examples:

| Symptom | Cosmon-ward? | Why |
|---|---|---|
| Runtime resurrects 14 stale pendings | ✅ yes | Invariant absent: Autonomous regime had no backlog-sanity precondition. ADR-048. |
| Spawn timer conflates two physical tests | ✅ yes (after N=2) | Primitive imprecise: gate measures something the operator did not name. Chronicle now, ADR after second occurrence. |
| Pilot forgot to add `--blocked-by` | ❌ no | Pilot ergonomic. Fix: better briefing, better `cs nucleate --help`. |
| Galaxy-local script needs a different state path | ❌ no | Galaxy-local convention. Fix: `.cosmon/config.toml` in the galaxy. |
| Two pilots disagreed on a workflow | ❌ no | Discipline question, not a primitive. |

The qualifier matters because **cosmon-ward is not free**. Each
cosmon-ward molecule consumes triage attention, may require a full
`idea-to-plan` cycle, and competes with first-order cosmon work. The
discipline is *don't silent-patch real cosmon defects*, not *promote
every annoyance to an ADR*.

### 3. Mechanics — citation and follow-up

When an application-site chronicle names a cosmon pathology:

- **Cite this ADR.** Reference by relative path
  (`~/dev/projects/cosmon/docs/adr/049-cosmon-ward-feedback-flow.md`)
  or absolute repo URL once one exists. The citation itself is the
  declaration that the rule was followed.
- **Nucleate the cosmon-ward molecule.** From any working directory,
  `cs nucleate ... --kind issue --var topic="..."` against the cosmon
  state-store (the `cs` binary uses walk-up discovery; explicit
  `--state-dir ~/dev/projects/cosmon/.cosmon/state` works from any
  galaxy).
- **Cross-link.** The application-site chronicle links to the cosmon
  molecule's id; the cosmon molecule's `prompt.md` cites the
  application-site chronicle file path.

This is the same shape as the cross-galaxy edges discipline (see ADR
on that topic): the durable artifact is a typed reference between
galaxies, not a mailbox or a notification.

## Consequences

**Positive.**

- **Closes the silent-erosion failure mode.** A pilot who follows
  the rule cannot pretend cosmon is fine while their workaround
  pile grows. The reactor learns mechanically rather than relying on
  pilot virtue.
- **Generates a legible cosmon-ward signal stream.** Triage of
  cosmon-ward issues becomes a first-class workflow: `cs ensemble
  --tag cosmon-ward` (a future tag convention) produces the queue of
  upstream-influenced architectural work.
- **Anchors future chronicles.** `mailroom`, `showroom`,
  `sandbox`, and any future galaxy can cite a stable
  ADR id rather than quote a chronicle paragraph or a global config
  rule. Citation density across galaxies grows, and the rule becomes
  *findable* by anyone reading any galaxy's docs.
- **Confirms the chronicled principle has structural weight.** The
  *Le réacteur apprend* chronicle moves from *narrative* to *binding
  architectural decision*; the principle is now part of cosmon's own
  rule corpus, not just one pilot's discipline.

**Negative.**

- **Adds latency between discovery and resolution.** A silent in-app
  patch is faster than a cosmon-ward molecule + triage + (possibly)
  ADR + (possibly) impl task. The trade is intentional: latency in
  exchange for non-erosion of the core.
- **Cosmon-ward molecules require triage attention.** Without
  curation (see the temperature-tag discipline in CLAUDE.md), the
  cosmon backlog accumulates cosmon-ward signals that crowd out
  first-order work. Mitigated by: temp tags, periodic
  `temp-review`, and the qualifier in §2.
- **Potential for over-application.** A pilot who reads this rule
  too literally may file cosmon-ward issues for ergonomic friction.
  The qualifier table in §2 is the canonical reply to that drift.

**Neutral.**

- No code change. No CLI change. No state-store change. This ADR
  encodes a discipline that the `cs` CLI already supports
  natively — the rule binds *what gets nucleated*, not *how
  nucleation works*.

## References

- **Inaugural chronicle**: an internal chronicle
  §*2026-04-17 nuit — Le réacteur apprend de ce qu'il brûle*. Long-form
  narrative rationale; the source from which this ADR is promoted.
- **First binding instance**: [ADR-048](048-backlog-sanity-invariant.md)
  (backlog-sanity invariant). The convoy cascade → cosmon-ward signal →
  idea → plan → ADR cycle ran end-to-end and proves the loop closes.
- **Observational instance**: an internal chronicle
  + `task-20260417-a588`. N=1 case where the cosmon-ward response is
  *publish observation, wait for second occurrence*; equally legitimate.
- **Global pilot discipline**: `~/.claude/CLAUDE.md` Core Rules entry
  *Cosmon-ward feedback flow*. The same rule, applied at every
  Claude session boot.
- **Cross-galaxy etiquette**: the *silent-ignore is forbidden* family of
  rules across galaxies. This ADR adds the cosmon-ward direction.
- **Temperature-tag discipline**: CLAUDE.md §*Molecule Temperature Tags*
  — the curation mechanism that prevents cosmon-ward backlog drift.
