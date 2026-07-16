# Parity Cliff — Local Leg — 2026-06-14

**Molecule:** `task-20260614-368b` · **Parent:** `delib-20260614-61f9` (diversify-now)
**Scope (operator override):** LOCAL adapter only. Mistral leg deferred (no API key).
**Question answered:** *How big is the gap between Claude 4.x and a sovereign
local model TODAY, for cosmon's actual workload?*

This is **measured evidence, not a guess**. Every number below comes from a
run against the live Ollama install on this machine, through the **same
`/v1/chat/completions` path the `local` adapter actually uses** (`spawn_local_session`
→ `OpenAIProvider` → `cosmon-agent-harness::run_loop`).

---

## The one-paragraph answer (for the operator)

The cable carries the *light* load and most of the *medium* load — but it
visibly sags on the heavy load, and it snaps at the true floor.

- **Orchestration (nucleate / evolve / tackle / done):** zero gap. It spends
  no model tokens, so it survives a total Claude lockout untouched. **100%.**
- **A simple worker molecule** (`cs tackle --adapter local`, "write a haiku"):
  **ran end-to-end and completed** on `qwen3:8b`. Synthesis written. No rescue.
- **A real deep-think synthesis** (the heaviest load — replayed `delib-20260614-61f9`):
  the local model **produces the right shape** — all 5 panelists named, coverage
  table, dated recommendation — **but at ~¼ the depth of Claude, and it silently
  drops the one check that makes a deliberation worth running** (the
  anti-groupthink cross-check). On the bigger 32B model it was *worse* on the
  thing that matters: it flattened 3 of 5 distinct experts into the same
  copy-pasted paragraph.
- **The true local floor** (a 3B model, the "sovereign laptop" case): tool calls
  time out ~half the time. Agentic work is effectively impossible there today.

**turing's a-priori verdict — "NOT substitutable today" for cosmon's agentic
spine — is empirically CONFIRMED.** But the failure is *graceful degradation*
(completes-but-shallow) on the Ollama/Qwen mid tier, not the *catastrophic
silence* carnot's table predicted for "local". The catastrophe only appears at
the genuine 3B floor.

---

## The numbers

### A. Tool-call validity (the oracle-substitutability core)

Harness-faithful probe: real `SYSTEM_PROMPT`, the v0 worker tool set
(`read_file / edit_file / write_file / exec_command / find_file / grep / list_dir`),
`tool_choice="auto"`. Four tasks of rising agentic demand; each graded on
(1) emit a *structured* tool_call with parseable JSON args naming a real tool
with required params, and (2) take a sane next step after a synthetic tool result.

| Model (cosmon tier) | turn-1 valid | turn-2 continue | full multi-step chain | latency / turn | hard timeouts (240s) |
|---|---|---|---|---|---|
| **qwen2.5:32b** (Mid) | 4/4 | 4/4 | 2/2 | 4–22 s | 0 |
| **qwen3:8b** (Mid) | 3/4 | 3/3 | 2/2 | 9–30 s | **1** |
| **llama3.2 3B** (true Local) | 2/4 | 2/2 | 2/2 | 1–**204 s** | **2** |

Reading it: on the *single turn-pair*, the Qwen mid-models are **genuinely good**
at structured tool-calling — better than carnot's table assumes. The damage is in
the **tails**: one 240-second timeout in four calls. Over a real 20-turn agentic
loop, a per-turn hard-failure rate of even 5–10% compounds far below turing's
**≥99% sequence-validity** bar. The 3B floor is already unusable: 50% timeout rate,
a 204-second tool call.

### B. Deep-think synthesis replay (heaviest load: LongContext + reasoning)

Fed the **exact** panel context that produced `delib-20260614-61f9` — 5 persona
responses + frame + prompt, **24,155 tokens** — and asked for the synthesis.

| | **Claude 4.x (baseline)** | **qwen3:8b** | **qwen2.5:32b** |
|---|---|---|---|
| Completed | ✓ | ✓ | ✓ |
| Output size | **18,802 chars** | 4,504 (**24%**) | 4,884 (**26%**) |
| Context truncated? | no | **no** (24.2k accepted) | **no** (24.2k accepted) |
| 5 personas named | ✓ | ✓ | ✓ |
| Coverage table | ✓ *(with verdict tokens)* | ✓ *(generic ✅/❌)* | ✓ *(generic ✅/❌)* |
| Per-persona **distinctiveness** | ✓ 5 distinct verdicts | ~ partial | ✗ **3/5 identical copy-paste** |
| Substitution-hypothesis check | ✓ real, per-persona | ✗ asserted not done | ✗ asserted not done |
| Pure-generation wall-clock | ~60–90 s (est.) | **233 s** | **711 s** |

**Wall-clock:** the local deep-think synthesis is **~2.5–4× slower (8B)** to
**~8–12× slower (32B)** than Claude *per generation* — and that is the optimistic
number, because it ignores the structural multiplier below.

### C. The structural wall-clock multiplier nobody prices in

Claude ran the 5-persona panel **in parallel** (panel step: ~12 min wall for 5
agents). A single local GPU has **no panel parallelism** — five personas run
*sequentially*. A faithful full local deep-think (`5 persona gens + 1 synthesis`,
each a ~4-min heavy gen on 8B) is **~25–30 min of model time** for what Claude
delivered in a 21-min wall-clock with concurrent dispatch — and at a quarter of
the depth. Bigger local model = *worse* here (32B was 3× slower than 8B).

---

## Mapping to the predictions

### carnot's exergy table (`crates/cosmon-provider/src/degradation.rs`)

The table is **validated — with one taxonomy correction the operator must hear.**

The Ollama adapter advertises `max_context = 32_768, supports_tools = false`.
Run that through cosmon's own `Capabilities::degradation_tier()`:

```
supports_tools (false) || max_context >= 16_384  →  DegradationTier::Mid
```

**So the "local" adapter, as actually wired, is a MID-tier backend in cosmon's
own taxonomy — not Local.** At Mid the table predicts:

| VerbClass | Predicted @ Mid | Measured (Qwen via Ollama) | Match |
|---|---|---|---|
| ControlPlane | Reliable | Reliable (0 tokens) | ✓ |
| FreeformGeneration | Reliable | Reliable (haiku completed) | ✓ |
| StructuredExtraction | Reliable | ~Reliable (tail timeouts) | ✓ |
| MultiStepAgentic | **Degraded** | **Degraded** (completes, thin, breach) | ✓ |
| LongContext | **Degraded** | **Degraded** (24k held, ¼ depth) | ✓ |

The `Unavailable` cells carnot predicted for **MultiStepAgentic/LongContext
"on local"** were measured at the **true 3B Local floor** (llama3.2): tool
timeouts → effectively unavailable. ✓

**The trap:** "local" the *word* means two different things. The operator means
*deployment* (sovereign, on my hardware). cosmon's `degradation_tier()` means
*capability* (read off the advertised window). A 32B model on your own GPU is
**Mid-tier-Degraded, not Local-Unavailable** — it *will* answer, just shallower
and slower. Do not assume "local = it can't". Assume "local = it degrades, and
the degradation is invisible unless you measure depth, not just exit code."

### turing's oracle-substitutability bar

| Criterion | Bar | Measured (mid tier) | Pass? |
|---|---|---|---|
| Tool-call sequence validity | ≥ 99% | ~90–100% on n=4 *single* pairs; tail timeouts sink multi-turn far below | **✗** |
| Zero invariant-breach escapes | 0 | **≥1**: substitution-check dropped; persona-distinctiveness collapsed (32B) | **✗** |
| Completion within 5pp of Claude | ≤ 5pp | simple tasks ~match; deep-think completes but at 24% depth + breach | **✗ (heavy)** |

**Verdict: NOT substitutable today** for the deliberation/mission spine.
Confirmed empirically, exactly as turing predicted a priori.

---

## Tasks-impossible count (the operator's explicit metric)

- **Hard-impossible on the mid tier (Qwen):** **0** of the tested workloads
  outright failed to terminate. Everything produced *an* output.
- **Invariant-breach ("possible but wrong in a way Claude isn't"):** **2 distinct
  breaches**, both on the heaviest load and both *silent* (no error, valid-looking
  artifact):
  1. **Substitution-hypothesis cross-check dropped.** The frame demanded a
     per-persona adversarial falsifier ("did buterin dodge the hard question into
     abstract exit-theory?"). Both local models *claimed* it was fine instead of
     *doing* it. This is the anti-groupthink (janis) machinery — the reason
     deep-think exists. Losing it silently is the worst kind of loss.
  2. **Persona flattening (32B).** Three of five distinct experts were summarized
     with a *byte-identical* copy-pasted paragraph. A deliberation whose panelists
     all say the same thing in the same words is theater — the diversity invariant
     is breached.
- **Effectively-impossible at the true 3B Local floor:** sustained agentic loops
  (50% tool-call timeout rate). MultiStepAgentic + LongContext are off the table.

---

## Pre-staged degradation routing (DRAFT — do not implement)

Per the carnot scope-guard: **do not re-engineer the deep-think/mission prompts
to make the weak model pass.** Measure the gap (done above), and *pre-stage the
routing decision* so that the day the export lock closes on Claude 4.x, the fleet
degrades by policy instead of by panic. Sketch only:

1. **Route by `VerbClass × DegradationTier`, not by adapter name.** The table in
   `degradation.rs` already exists and is already correct. Wire `cs tackle` /
   `cs run` to *consult* `reliability_for(class)` and:
   - `Reliable` → dispatch normally.
   - `Degraded` → dispatch into a **guard-railed mode** (see #2), and **stamp the
     artifact** `degraded_oracle: <tier>` so no downstream consumer mistakes a
     shallow synthesis for a full one.
   - `Unavailable` → **refuse and surface**, do not emit confident garbage. A
     deep-think on a 3B floor should fail loud, not produce a flat panel.

2. **Guard-railed mode for `MultiStepAgentic` on a weak tier** (no prompt rewrite):
   - **GBNF / tool-schema-bound decoding** for every structured step (the table
     already says StructuredExtraction is only `Degraded`-safe *behind a grammar*).
   - **Mandatory checklist gates** the deterministic harness enforces, not the
     model: e.g. deep-think `synthesize` cannot advance until the coverage table
     has one row per `Qn` *with a non-empty verdict token* and the
     substitution-check table has one row per panelist. The model can't skip what
     the harness refuses to accept. This is **mechanism, not persuasion** — the
     cosmon way (`§8b propose verification, don't impose intelligence`).
   - **Lower per-call timeout + retry budget** tuned to the tail latency measured
     above (the 240s outliers are the real killer of long loops).
   - **No-parallelism awareness:** on a single local GPU, a panel must be costed
     sequentially. Either reduce panel size on the weak tier (3 personas, not 5)
     or warn the operator of the ~6× wall-clock before dispatch.

3. **Stamp every degraded artifact.** The single most important pre-stage: a
   `synthesis.md` produced on a Degraded tier must carry a visible banner. The
   failure mode that bites is not "it broke" — it's "it looked fine and was ¼ as
   deep, and nobody noticed."

The routing primitive to build is **one consultation call** (`reliability_for`)
plus **one artifact stamp** plus **harness-side acceptance gates**. None of it
touches a persona prompt. That is carnot's "pre-stage the routing decision, not
the rewrite," made concrete.

---

## What was run (reproducibility)

- `cs tackle --adapter local` end-to-end: `cargo test -p cosmon-cli --test
  demo_local_acceptance -- --ignored` with `COSMON_LOCAL_DEMO=1`,
  `COSMON_LOCAL_MODEL=qwen3:8b` → **passed** (190s loop; synthesis.md written;
  zero `claude` subprocess).
- Tool-call probe: `scratch/probe_local.py {qwen3:8b, qwen2.5:32b, llama3.2}` →
  `scratch/probe_results.json`.
- Deep-think replay: `scratch/replay_deepthink.py {qwen3:8b, qwen2.5:32b}` →
  `scratch/synth_*.md`, `scratch/replay_results.json`.
- Baseline: `delib-20260614-61f9/synthesis.md` (Claude 4.x), `log.md` (timings).
- Evidence copies of the probe outputs are saved alongside this report.

**Models tested:** all non-US open weights (Qwen = Alibaba; Llama = Meta open
weights, run fully offline via Ollama). No request left `localhost:11434`.
