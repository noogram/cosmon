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
| `cross-provider-committee` | the provider-diverse jury (claude + codex-sol) = the double clean-room review | `../../.cosmon/formulas/cross-provider-committee.formula.toml` (ADR-147) |
| `bug-closure` | the CLOSED/REOPEN verdict over the full semantic surface = the closure gate | `../../.cosmon/formulas/bug-closure.formula.toml` |
| `task-work` | the agentic gate legs (verdict contract in each node topic) | `../../.cosmon/formulas/task-work.formula.toml` |

Only **two** formulas are genuinely new (they ship in `formulas/`):

- `clean-room-repro` — the G2 reproduction gate (deterministic red, frozen before
  the fix, differential refutation, false-green/false-red modes; emits verdict.json).
- `converge-clean-room` — the §6bis double-engine convergence, rendered as **two
  immutable verdicts** over `cross-provider-committee`: an INITIAL verdict and —
  only if it found something AND the fix moved the target tree sha — one
  CONFIRMATION verdict. It reads the seats' verdict.json, computes CLEAN as the
  conjunction over the **ballot-carrying** seats, and fail-closes on an absent /
  unparseable / undelivered verdict into a **VOID** verdict that does not advance
  the state. No round counter, no round ledger, no third verdict. `while` is
  deliberately **not** composed — it was the N-round machinery (operator decision D1
  amendment 1 on `delib-20260729-1d4e`).

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
                       converge (§6bis, EMERGENT — the two-verdict clean-room  ─┬─→ rehearsal(G9) ─┐
                       replaces G6 breaker + G7 judge)                          └─→ dissent(§9) ───┤
                                                                                                    ▼
                                                                            release(G10) → confirm(G11)
```

**CLEAN = ballot-weighted conjunction**: every **ballot-carrying** seat
(`on_ballot`, `diversity_weight > 0`) must return CLEAN in the same verdict, and the
ballot-carrying set must be non-empty. A single FINDINGS from a ballot-carrying seat
blocks and the fix corrects the UNION of the ballot-carrying reports; an off-ballot
seat's findings are recorded as residuals and block nothing on their own, because an
off-ballot CLEAN certifies nothing. A verdict that measured nothing — absent,
malformed, unparseable, or a seat not `delivered` — is **VOID**: it does not advance
the state, the seat is re-dispatched once, and a second void collapses on
`verdict-plumbing`. Every non-CLEAN outcome is `blocked` + human escalation with a
typed `exit_reason`, NEVER a silent pass.

## The seal (TLC-verified green)

`spore.tla` + `spore.cfg` model the diamond gate DAG + the bounded convergence and
discharge four properties: **Termination** (the convergence is bounded structurally
by its two named verdict states; the DAG is acyclic; a blocked convergence cascades
to a terminal state, no spin), **GateFailClosed** (no gate promotes on absent/failing
evidence; release SHIPs only when every upstream gate PASSED, the convergence is
CLEAN, and the dissent field is non-empty), **DeterministicParametrization**, **NoResourceCollision**.

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
cs run --resident --poll-interval 5 <germinated-root>       # absorbs the converge verdicts' children
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
