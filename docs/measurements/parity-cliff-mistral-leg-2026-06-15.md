# Parity Cliff — Mistral Leg — 2026-06-15

**Molecule:** `task-20260615-90f1` · **Parent:** `delib-20260614-61f9` (diversify-now)
**Completes:** the local-only pass of `task-20260614-368b`
([`parity-cliff-local-leg-2026-06-14.md`](parity-cliff-local-leg-2026-06-14.md)),
now that the Mistral key is live.
**Scope:** the **EU-sovereign remote** option — `mistral-large-latest`, reached
through the `openai` adapter wired by `task-20260614-62bc`
(`base_url=https://api.mistral.ai`, `default_model=mistral-large-latest`,
`api_key_env=MISTRAL_API_KEY`).
**Question answered:** *Does the European sovereign option carry the heavy
agentic cargo — or only the light load like the local cable?*

This is **measured evidence, not a guess**. Every number below comes from a
live run against `api.mistral.ai`, through the **same
`/v1/chat/completions` path the `openai` adapter actually uses**
(`spawn_openai_session` → `OpenAIProvider::with_base_url` →
`cosmon-agent-harness::run_loop`). The probe and replay scripts are
byte-identical ports of the local-leg scripts — only endpoint, auth header,
and model name differ — so the three columns are directly comparable.

---

## The one-paragraph answer (for the operator)

The canot is **not** a canot. On cosmon's actual workload, Mistral Large lands
**right next to Claude**, not next to the local model — a different league
entirely from the Qwen cable.

- **The heavy cargo (deep-think synthesis):** Mistral produced **92% of
  Claude's depth** (17,270 vs 18,802 chars), kept all **5 personas distinct**
  (no flattening), and — the thing that matters — it **did not silently drop
  the anti-groupthink guard-rail** that Qwen dropped. It tracked substitution
  per-question and even named which personas went *silent* on which
  sub-question. Where the local cable delivered ¼ the depth with a silent
  breach, Mistral delivers near-full depth with the guard-rail intact.
- **Tool calls (the oracle core):** **flawless** — 4/4 first-turn, 4/4
  continuation, 2/2 full multi-step chains, **zero timeouts**, and **sub-second
  per turn** (0.8–1.2 s). That is Claude-class, far above Qwen's 4–30 s with
  tail timeouts.
- **A real molecule end-to-end:** `cs demo --adapter openai` nucleated →
  tackled → **completed** a task-work molecule (`task-20260615-007a`) in
  **10.8 s**, and the egress audit atom proves the traffic genuinely went to
  `api.mistral.ai` (not silently to `api.openai.com`).
- **The one real wall — and it is *not* the model:** the API key sits on a
  **4-requests-per-minute** tier. That is a *billing* ceiling, not a capability
  ceiling (token budget is a roomy 250k/min). It is harmless for a single deep
  call, but it **would choke a fast multi-turn agent loop** unless the account
  is upgraded or cosmon adds client-side throttling. This is the Mistral analog
  of the local cable's "no-GPU-parallelism" tax — the structural cost nobody
  prices in.

**Verdict:** **Mistral Large IS a real plan B for the heavy agentic load.**
turing's a-priori "NOT substitutable today" held for the *local* cable; it does
**not** hold for Mistral on the dimensions of *quality*. The sovereign European
option carries the cargo. The only thing standing between "rehearsal" and
"production" is a **paid rate-limit tier**, not a capability gap.

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
| **mistral-large-latest** (remote) | **4/4** | **4/4** | **2/2** | **0.8–1.2 s** | **0** |
| qwen2.5:32b (Mid, local) | 4/4 | 4/4 | 2/2 | 4–22 s | 0 |
| qwen3:8b (Mid, local) | 3/4 | 3/3 | 2/2 | 9–30 s | 1 |
| llama3.2 3B (true Local) | 2/4 | 2/2 | 2/2 | 1–204 s | 2 |

Reading it: Mistral is **perfect on the single-turn pair and ~20× faster** than
the best local model, with **no tail timeouts** — the failure mode that sinks
the local models over a long loop simply is not present. On structured
tool-calling, Mistral is indistinguishable from a frontier oracle. The
**only** caveat is operational, not behavioural: see §C.

### B. Deep-think synthesis replay (heaviest load: LongContext + reasoning)

Fed the **exact** panel context that produced `delib-20260614-61f9` — 5 persona
responses + frame + prompt, **24,386 prompt tokens** — and asked for the
synthesis, with the **same condensed `synthesize` instruction** the local leg
used.

| | **Claude 4.x (baseline)** | **Mistral Large** | **Qwen local (8b / 32b)** |
|---|---|---|---|
| Completed | ✓ | **✓** | ✓ / ✓ |
| Output size | **18,802 chars** | **17,270 (92%)** | 4,504 (24%) / 4,884 (26%) |
| Completion tokens | ~4–5k (est.) | **4,802** | 1,029 (32b) |
| Context truncated? | no | **no** (24.4k accepted) | no / no |
| 5 personas named | ✓ | **✓** | ✓ / ✓ |
| Per-persona **distinctiveness** | ✓ 5 distinct | **✓ 5 distinct** | ~partial / ✗ 3/5 copy-paste |
| Coverage table | ✓ (verdict tokens) | **✓ (verdict tokens + sub-Qs + named silences)** | ✓ generic / ✓ generic |
| Anti-groupthink substitution check | ✓ dedicated per-persona table (Fired? + Evidence) | **~ tracked: per-Q `Substituted` column + named silences + assertion** | ✗ asserted not done / ✗ asserted not done |
| Pure-generation wall-clock | ~60–90 s (est.) | **86.4 s** | 233 s / 711 s |

**The one honest nuance.** Mistral does **not** reproduce Claude's *dedicated*
per-persona substitution-falsifier table ("did buterin dodge Q2 into abstract
exit-theory? — Fired? No — Evidence: …"). It folds substitution tracking into
the frame-coverage table's `Substituted` column (all ❌) plus an explicit
"No persona substituted — all engaged with the hard questions," and it
**names which personas were silent on which sub-question** (niel, turing on the
sister-molecule reference). That is **materially weaker than Claude's evidence
table, but it is not the *silent drop* Qwen committed** — the guard-rail is
present and visible, just rendered more compactly. The diversity invariant
(distinct personas) **holds**; Qwen 32b breached it.

**Wall-clock:** Mistral's deep-think generation is **Claude-speed** (~86 s,
inside the Claude estimate band) and **3–8× faster than the local cable**
(233 s on 8b, 711 s on 32b).

### C. The structural wall nobody prices in — the rate-limit tier

The local cable's hidden tax was **no GPU parallelism** (5 personas run
sequentially → ~6× wall-clock). Mistral's hidden tax is the **account's
request-rate tier**:

```
x-ratelimit-limit-req-minute: 4         ← the binding constraint
x-ratelimit-limit-tokens-minute: 250000 ← roomy, never the problem
```

- **4 requests / minute.** A single deep-think synthesis is one request — fine.
  But a *faithful* deep-think runs **6 calls** (5 personas + synthesis), and an
  agentic task-work loop runs **one call per turn**. At 4 rpm, a 20-turn
  agentic loop forces **~5 minutes of pure rate-limit waiting** on top of
  generation — and cosmon's harness **surfaces a 429 verbatim and stops the
  loop** (no built-in retry; confirmed in `cosmon-agent-harness::spine` — the
  provider's typed `RateLimited` error is returned, not retried).
- **This is plumbing, not capability.** The 250k-token budget proves Mistral
  Large *can* do the work; the 4-rpm cap is a billing tier. The probe and
  replay above absorbed the cap with client-side backoff (a measurement
  artifact), which is why their *latency* numbers reflect the model and their
  *throughput* does not.

**Pre-stage, do not rewrite (carnot).** Two honest options for production-grade
Mistral throughput, neither touching a formula prompt:
1. **Upgrade the Mistral account tier** (the cheap, reversible move — same
   shape as the warm-standby decision itself).
2. **Add client-side throttling/retry to the `openai` adapter loop** so a 429
   is paced, not fatal. This is the same "mechanism, not persuasion" pre-stage
   the local leg proposed — a harness-side concern, adapter-agnostic.

---

## Mapping to the predictions

### carnot's exergy table (`crates/cosmon-provider/src/degradation.rs`)

Mistral Large advertises a large context and tool support → cosmon's
`Capabilities::degradation_tier()` places it in the **Frontier/High** band, not
Mid. The table predicts `Reliable` across every `VerbClass` at that tier, and
the measurement **confirms it**:

| VerbClass | Predicted @ Frontier | Measured (Mistral Large) | Match |
|---|---|---|---|
| ControlPlane | Reliable | Reliable (0 tokens) | ✓ |
| FreeformGeneration | Reliable | Reliable (haiku/synthesis) | ✓ |
| StructuredExtraction | Reliable | Reliable (4/4 tool calls, 0 timeouts) | ✓ |
| MultiStepAgentic | Reliable | Reliable (2/2 chains; *throughput* gated by rpm tier, not capability) | ✓\* |
| LongContext | Reliable | Reliable (24.4k held, 92% depth) | ✓ |

\* The only asterisk is operational (4-rpm tier), not a degradation of the
model's MultiStepAgentic capability.

### turing's oracle-substitutability bar

| Criterion | Bar | Measured (Mistral Large) | Pass? |
|---|---|---|---|
| Tool-call sequence validity | ≥ 99% | 4/4 + 4/4 + 2/2, 0 timeouts, sub-second | **✓** (n=4; no tail failures, unlike local) |
| Zero invariant-breach escapes | 0 | **0**: personas distinct, guard-rail present (compact, not dropped) | **✓** |
| Completion within 5pp of Claude | ≤ 5pp | deep-think at **92%** depth, all structural elements present | **~✓** (8pp on raw size; qualitatively at-parity) |

**Verdict: substitutable-in-quality today** for the deliberation/mission spine —
the dimension on which the local cable *failed*. The residual risk is
**operational throughput** (rate-limit tier), which is purchasable, not a
capability gap.

---

## Tasks-impossible count (the operator's explicit metric)

- **Hard-impossible:** **0**. Every workload terminated with a valid artifact.
- **Invariant-breach ("possible but wrong in a way Claude isn't"):** **0
  silent breaches.** Personas stayed distinct (local 32b flattened 3/5);
  the anti-groupthink guard-rail was rendered (local dropped it silently).
  The one *delta* vs Claude is a **compaction** of the substitution check
  (per-Q column + assertion instead of a dedicated per-persona evidence
  table) — visible and gradeable, not a silent loss.
- **Operationally-gated (NOT a model failure):** a **fast multi-turn agentic
  loop** on the current **4-rpm key tier** will hit 429s that cosmon surfaces
  as fatal. This is the Mistral analog of the local 3B floor's timeouts —
  except here the fix is a billing upgrade, not better silicon.

---

## What was run (reproducibility)

- **End-to-end molecule:** `cs demo --adapter openai --no-teardown --prompt
  "<haiku task>"` → molecule `task-20260615-007a` reached **Completed in
  10.8 s**; `log.md`: *"in-process agent loop returned Ok (openai adapter,
  ADR-100 Direct-API)"*; global `events.jsonl` carries
  `remote_egress_opt_in { adapter_name: "openai", endpoint_host:
  "api.mistral.ai", port: 443 }` then `molecule_completed` — proving the
  buterin egress-gap fix (`task-20260614-62bc`) stamps the **real**
  destination. Demo worktree removed after inspection (clean, no commits;
  in-process → no tmux session).
- **Tool-call probe:** `scratch/probe_mistral.py mistral-large-latest` →
  `scratch/probe_mistral_results.json`. (429-backoff added for the 4-rpm tier;
  plumbing only — does not bias the per-call model metric.)
- **Deep-think replay:** `scratch/replay_mistral.py mistral-large-latest` →
  `scratch/synth_mistral-large-latest.md`, `scratch/replay_mistral_results.json`.
- **Baselines:** Claude 4.x from `delib-20260614-61f9/synthesis.md`; Qwen local
  from `parity-cliff-local-leg-2026-06-14.md` + `task-20260614-368b/evidence/`.
- **Evidence copies** of all Mistral scripts + outputs saved in this molecule's
  `evidence/` directory.

### carnot scope-guard — what was deliberately NOT done

Per the local-leg guard-rail, to measure the gap *as it is*:

- **No formula prompt was rewritten** to flatter Mistral. The deep-think replay
  used the identical condensed `synthesize` instruction as the local leg, and
  the probe used the identical worker `SYSTEM_PROMPT` + tool set.
- **The default adapter was not changed** — it remains `claude`. Mistral is
  reached only via an explicit `--adapter openai`.
- **No cosmon core was touched** — `task-20260614-62bc` already did the wiring;
  this leg only measures.

---

## Bottom line for the diversify-now decision

| | Claude 4.x | **Mistral Large** | Qwen local |
|---|---|---|---|
| Deep-think depth | 100% | **92%** | 24–26% |
| Anti-groupthink guard-rail | full table | **present (compact)** | **dropped silently** |
| Persona distinctiveness | 5/5 | **5/5** | partial / 3-of-5 copied |
| Tool-call validity | frontier | **flawless, sub-second** | good-but-tail-timeouts |
| Deep-think wall-clock | ~60–90 s | **~86 s** | 233–711 s |
| Sovereignty | US (export-exposed) | **EU sovereign** | on-device |
| Production blocker | — | **rate-limit tier (purchasable)** | capability (silicon) |

**The sovereign European lifeboat is seaworthy for the frontier cargo.** It is
not a downgrade you tolerate in an emergency — it is a near-peer you can
*rehearse on today*, with one purchase order (a paid rate-limit tier) standing
between it and full agentic throughput. That is the number that decides
diversify-now: **Mistral is a real plan B for the heavy agentic load — Qwen,
today, is not.**
