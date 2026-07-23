# `cosmon-dev` — the robust-dev spore (the first spore of the cosmon repo)

`cosmon-dev` is the mission that turns **one issue reported by an external tester**
into a **deterministic red reproduction** (a validation gate), a **smallest fix**,
a **double clean-room review by two different provider families iterated until
CLEAN**, and a **release** — with **no agent pushing to any remote**.

It is **dogfooding**: unlike a shareable spore that travels to a stranger's machine
(math-attack in the sporarium repo), `cosmon-dev` lives *inside* cosmon and
composes the repo's own primitives by relative path.

## Guiding principle: reuse, do not reinvent

The spore **wires** existing cosmon primitives; it rewrites none:

| primitive | role here | reference |
|-----------|-----------|-----------|
| `while` | the iterate-until-condition loop; `max_iterations` = `max_rounds` | `../../.cosmon/formulas/while.formula.toml` |
| `cross-provider-committee` | the provider-diverse jury (claude + codex-sol) = the double clean-room review | `../../.cosmon/formulas/cross-provider-committee.formula.toml` (ADR-147) |
| `bug-closure` | the CLOSED/REOPEN verdict over the full semantic surface = the closure gate | `../../.cosmon/formulas/bug-closure.formula.toml` |
| `task-work` | the agentic gate legs (verdict contract in each node topic) | `../../.cosmon/formulas/task-work.formula.toml` |

Only **two** formulas are genuinely new (they ship in `formulas/`):

- `clean-room-repro` — the G2 reproduction gate (deterministic red, frozen before
  the fix, differential refutation, false-green/false-red modes; emits verdict.json).
- `converge-clean-room` — the §6bis double-engine convergence loop that **composes**
  `while` + `cross-provider-committee`: each round runs review-claude ‖
  review-codex-sol, reads BOTH verdict.json, CLEAN∧CLEAN debloque, else re-nucleates
  fix(union)+reviews forward; fail-closes on absent verdict; blocked+escalate at
  max_rounds.

## Layout

```
spores/cosmon-dev/
├── README.md              # this file
├── spore.toml             # the wiring: params + fleet + formula aliases + DAG + seal
├── spore.tla / spore.cfg  # the TLC-VERIFIED seal (4 properties, green — see below)
├── mission-template.md    # the parameterized briefing
├── formulas/
│   ├── clean-room-repro.formula.toml      # NEW — G2 reproduction gate
│   └── converge-clean-room.formula.toml   # NEW — §6bis double-engine loop
├── clean-room/            # the chambre-blanche container discipline (§4)
│   ├── README.md          #   one image, three postures, disposable login, two net modes
│   ├── Dockerfile         #   debian bookworm-slim by digest, claude-code pinned, autoupdate off
│   └── scripts/           #   run-clean-room.sh · disposable-login.sh · assert-no-host-mounts.sh
└── repro/                 # the two red repro-contracts (§5 — the seeds)
    ├── README.md
    ├── contract-21-adapter-resolver.md            # #21 (resolver precedence, no LLM)
    ├── contract-20A-root-bypass-spawn.md          # #20A (root spawn, no LLM)
    └── contract-20B-prompt-write-outside-worktree.md  # #20B (fs containment, offline claude -p)
```

## The gate DAG (blueprint §3 — diamond, not pipeline)

```
trace (root+leaf, always-on sidecar)
intake(G0) → contract(G1) → reproduce(G2) ─┬─→ implement(G4) ─┐
                                           └─→ falsify(G3) ───┴─→ green(G5) → ci-gate(G8)
                                                                       │
                                                                       ▼
                       converge (§6bis, EMERGENT — the double clean-room loop, ─┬─→ rehearsal(G9) ─┐
                       replaces G6 breaker + G7 judge)                          └─→ dissent(§9) ───┤
                                                                                                    ▼
                                                                            release(G10) → confirm(G11)
```

**CLEAN = conjunction**: both review seats (claude AND codex-sol) must return CLEAN
in the same round. A single FINDINGS from either relaunches the loop; the fix
corrects the UNION of the two reports. At `max_rounds` exhaustion the mission goes
`blocked` + human escalation, NEVER a silent pass.

## The seal (TLC-verified green)

`spore.tla` + `spore.cfg` model the diamond gate DAG + the bounded convergence loop
and discharge four properties: **Termination** (the loop is bounded by max_rounds;
the DAG is acyclic; a blocked convergence cascades to a terminal state, no spin),
**GateFailClosed** (no gate promotes on absent/failing evidence; release SHIPs only
when every upstream gate PASSED, the loop is CLEAN, and the dissent field is
non-empty), **DeterministicParametrization**, **NoResourceCollision**.

Re-verify (any Java 11+; jar at `../../docs/specs/tla2tools.jar`):

```bash
cd spores/cosmon-dev
export TLA2TOOLS_JAR=../../docs/specs/tla2tools.jar
java -XX:+UseParallelGC -cp "$TLA2TOOLS_JAR" tlc2.TLC -workers auto -config spore.cfg spore.tla
# => Model checking completed. No error has been found.
```

## Run it

```bash
cd spores/cosmon-dev
cs spore validate spore.toml \
    --var issue="#21 --resident ignores COSMON_DEFAULT_ADAPTER" \
    --var affected_ref="v0.2.2" \
    --var upstream_version="0.2.2"
cs spore run spore.toml --var ... --allow-unchecked-seal   # released cs: TLC-verify not wired
cs run --resident --poll-interval 5 <germinated-root>       # absorbs the converge loop's rounds
```

## The limite dure (blueprint §8, honoured)

The spore edges **order** molecules; they do NOT prove a review passed or that two
seats had distinct identities or that a branch rule held. Every gate **emits and
validates** a machine-readable verdict and **fail-closes** on absent/malformed. The
spore germinates the **topology**; identities, credentials, and branch-protection
stay external human controls. No agent pushes to any remote.

## Named follow-ups (surfaced, not botched)

- **Warn-not-collapse** for a model pinned with no reachable adapter (blueprint §5
  adjacent nit to #21) — a germination/dispatch warning, not a collapse.
- **Typed-blocked, not auto-Enter** for an unapproved prompt (blueprint §5 adjacent
  nit to #20B) — an untyped auto-approval is a security fault; retire it from the
  autonomy claims and render a typed `blocked` state under a bound.
- **Re-pin the Dockerfile base digest + apt snapshot date** before the first real
  clean-room run (the committed Dockerfile carries placeholder pins to re-validate).
