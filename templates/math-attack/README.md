# `math-attack` — a shareable spore for attacking a hard conjecture

A **spore** is a fill-in-the-blanks template of a *whole* cosmon polymer: a
fleet, a set of per-node formulas, a parameter schema, and a DAG of typed
edges. You supply a conjecture, run one command, and cosmon germinates a full
proof/refutation pipeline whose central invariant is simple:

> **No target is ever called *proved* on an LLM's say-so.** A machine kernel
> (Lean's `lake build`) authors the verdict; the LLM only proposes proof terms
> and prose. This is the *LLM firewall*.

This document is the recipient quickstart: **validate → run → what to expect**.

---

## 0. Prerequisites

- You cloned `github.com/noogram/cosmon` and built the CLI (`cargo build -p
  cosmon-cli --bin cs`), so `cs` is on your `PATH`.
- You are inside a cosmon-initialized project (a `.cosmon/` exists — run
  `cs init` if not).
- Optional but recommended for a real kernel verdict: **Lean 4 + `lake`**
  installed. Without it, run with `formal_backend=none` (see below).

---

## 1. Validate — a dry run that germinates nothing

`validate` parses the spore, expands it against your parameters, and prints the
ordered list of `cs nucleate … --blocked-by …` calls it *would* make. Nothing
is written. Use it to see the shape of your attack before committing compute.

**Single-target** (the simplest case — one conjecture, one proof attempt):

```sh
cs spore validate templates/math-attack/spore.toml \
  --var subject="Firoozbakht's conjecture" \
  --var problem_statement="p(n+1)^(1/(n+1)) < p(n)^(1/n) for all n>=1, to be PROVEN or REFUTED, not assumed" \
  --var origin="Firoozbakht 1982" \
  --var formal_backend=none
# => spore: math-attack (v1) - 14 call(s)
```

**Multi-subquestion** (decompose the attack into parallel targets):

```sh
cs spore validate templates/math-attack/spore.toml \
  --var subject="Exponential-family stability" \
  --var problem_statement="The natural-parameter MLE map is globally Lipschitz on the mean-parameter polytope, to be PROVEN or REFUTED, not assumed" \
  --var subquestions="interior-strong-convexity,boundary-degeneracy,uniform-Lipschitz-constant" \
  --var formal_backend=lean \
  --var adversarial_corpus_min=15 \
  --var literature_anchors="brown1986exponential,wainwright2008graphical" \
  --var delivery=staged
# => spore: math-attack (v1) - 18 call(s)   (3 proof-attempts || 3 notebooks)
```

`--var list=a,b,c` splits on commas. `cs spore validate … --json` emits one
NDJSON line per call if you want to inspect it programmatically.

### Parameters

| Param | Type | Req | Default | Meaning |
|-------|------|-----|---------|---------|
| `subject` | string | **yes** | — | Short name of the conjecture. |
| `problem_statement` | string | **yes** | — | The verbatim conjecture, phrased *"to be PROVEN or REFUTED, not assumed"*. |
| `origin` | string | no | `""` | Provenance / poser / motivation. |
| `subquestions` | list\<string\> | no | `["main"]` | Attack targets. One entry ⇒ single-target; many ⇒ fan-out. **Never empty.** |
| `formal_backend` | enum `lean\|none` | no | `none` | `lean` ⇒ a real kernel leg gates the seal; `none` ⇒ Lean branch skipped, seal degrades honestly. |
| `adversarial_corpus_min` | int | no | `10` | Minimum false statements the red-team corpus must author. |
| `literature_anchors` | list\<string\> | no | `[]` | Seed citations for the source ledger. |
| `delivery` | enum `private\|staged\|public` | no | `private` | Delivery posture for the paper. |

**Why "single-target" is a 1-element list, not an empty one.** The spore parser
(ADR-140) fixes each node's kind — `fixed` or `fanout` — at parse time; a node
cannot flip between them based on a value. So the single-target case is modelled
as a one-element `subquestions` fan-out (the default `["main"]`), which
germinates exactly one proof attempt. Passing an *empty* list is rejected,
because a fan-out with nothing to range over is a typo, not an intention.

---

## 2. Run — germinate the real polymer

```sh
cs spore run templates/math-attack/spore.toml \
  --var subject="…" --var problem_statement="…" \
  --allow-unchecked-seal
```

### Why `--allow-unchecked-seal` is required (for now)

This spore declares a `[spore.seal]` block — it *claims* three safety
properties (`Termination`, `GateFailClosed`, `NoResourceCollision`). But the
TLC proof (`spore.tla` / `spore.cfg`) is **not written yet** — that is the
follow-up molecule. A sealed spore **fails closed**: `cs spore run` refuses
rather than pretend a proof ran. Passing `--allow-unchecked-seal` opts into the
risk, and the status line stays honest:

```
seal: present, NOT verified
```

It will **never** print `verified` until the `.tla` module exists and TLC
checks it. When the follow-up lands, drop the flag.

`cs spore run … --json` emits one NDJSON object per germinated molecule. After
germination, drive the DAG to completion with:

```sh
cs run <mission-or-root-id> --poll-interval 10
```

---

## 3. What artifacts to expect

The DAG runs 12 logical stages (fan-out stages multiply per subquestion).
`A || B` means the two run in parallel and the next stage waits for both.

```
decompose → source-ledger → concept-cards
          → [ proof-attempt || notebooks ]   (one each per subquestion)
          → skeptic → lean-skeleton
          → [ lean-probe || red-team-corpus ]
          → seal-gate → synthesize → write-paper
          → editorial-verdict → chronicle
```

| Stage | Produces | Role |
|-------|----------|------|
| `decompose` | `decompose.md` | Formal restatement, proof-obligation tree, strategies, falsifiability tests. |
| `source-ledger` | `source-ledger.md` | Bibliography: citekey + locator + exact statement per source. |
| `concept-cards` | `concept-cards/` | One card per load-bearing definition/lemma, pinned to a ledger row. |
| `proof-attempt` (×N) | `proof-attempt-i.md` | A rigorous prove-or-refute of one target. Never asserts truth. |
| `notebooks` (×N) | `notebook-i` + findings | Computational corroboration/refutation. Never *is* the proof. |
| `skeptic` | `faults.md` | Adversarial review; findings tagged BLOCKER/MAJOR/MINOR. |
| `lean-skeleton` | `lean/` or `skeleton.md` | `theorem … := by sorry` — the fidelity anchor. |
| `lean-probe` | `lean-probe-report.md` | `lake build` verdict: PROVED or UNPROVABLE_IN_BUDGET. |
| `red-team-corpus` | `corpus/` + coverage | ≥ `adversarial_corpus_min` FALSE statements the kernel must reject. |
| `seal-gate` | `verification-report.md`, `seal-verdict.md` | Three fail-closed legs (see below). |
| `synthesize` | `synthesis.md` | Proved / refuted / open, at what confidence. |
| `write-paper` | the paper (LaTeX/md) | Attribution: **Noogram**. Every cite traces to a ledger row. |
| `editorial-verdict` | verdict | Fail-closed SHIP or REWRITE. |
| `chronicle` | `docs/lore/CHRONICLES.md` | 0–3 entries, only if a principle was illuminated. |

### The seal gate's three legs (fail-closed)

1. **Kernel leg** — `lean-probe` reports `lake build` exit 0, grep-clean of
   `sorry`/`axiom`. With `formal_backend=none` this leg is honestly recorded as
   **DEGRADED**, not passed — the seal then rests on the skeptic + editor legs.
2. **Citation leg** — the `citation-audit` report shows zero unresolved
   `L3`/fabricated citations.
3. **Skeptic leg** — `faults.md` has zero residual `BLOCKER`s.

`SEALED` only if every applicable leg passes; otherwise `BLOCKED` with the
failing leg named. Nothing degrades to "pass" silently.

---

## 4. Export — a content-addressed bundle for sharing

```sh
cs spore export templates/math-attack/spore.toml --out dist/
# => bundle: blake3:…   (stable hash over spore.toml + every referenced formula/seal file)
# => astra:  dist/ro-crate-metadata.json   (RO-Crate descriptive layer, ADR-140 D6)
```

The bundle hash is deterministic: same bytes ⇒ same id. Share the hash to pin
exactly which version of the attack someone ran.

---

## 5. Files in this template

```
templates/math-attack/
  spore.toml                       the mission plan (params + DAG + seal)
  README.md                        this file
  formulas/
    task-work.formula.toml         generic agentic leg (most nodes)
    citation-audit.formula.toml    the seal's citation leg  (lifted, exp-families-stability)
    mycelium.formula.toml          the chronicle fold        (lifted, formal-research)
```

`spore.tla` / `spore.cfg` are **intentionally absent** until the TLC follow-up
molecule writes and verifies them. Until then: `--allow-unchecked-seal`.
