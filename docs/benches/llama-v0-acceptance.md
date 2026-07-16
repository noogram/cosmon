# llama v0 — acceptance gate (bench report)

**Molecule:** [`task-20260519-4e4a`](../../.cosmon/state/fleets/default/molecules/task-20260519-4e4a/)
**Parent delib:** [`delib-20260519-7044`](../../.cosmon/state/fleets/default/molecules/delib-20260519-7044/synthesis.md)
**Sibling tasks:** [`task-20260519-cfe0`](../../.cosmon/state/archive/2026/05/) (LlamaProvider adapter),
[`task-20260519-6a29`](../../.cosmon/state/) (ADR + chronicle, consumes
this verdict).

This file is the **one-shot artefact** that records what the local
llama.cpp adapter actually does on the operator's M4 Max. The Rust
harness lives at
[`crates/cosmon-provider/benches/llama_bench.rs`](../../crates/cosmon-provider/benches/llama_bench.rs);
the upstream cross-validation wrapper at
[`scripts/llama-bench.sh`](../../scripts/llama-bench.sh).

> **Reproducibility model.** The numbers below are recorded once, on a
> specific machine, with a specific build. Reproducibility is achieved
> by **context capture** (hardware revision, OS, llama.cpp SHA, GGUF
> hashes), not by re-running on CI. CI does not have a 70B model and
> no Metal GPU; the gate lives on the operator's laptop. The bench is
> opt-in via `cargo bench -p cosmon-provider --features llama` — it
> never runs by default.

---

## How to reproduce

```sh
# 1. Pre-cache the GGUFs the operator wants to test (skip what is missing —
#    the Rust harness emits a `missing_fixture` row for any unset var).
export COSMON_LLAMA_BENCH_GGUF_8B=~/.local/share/cosmon/models/by-name/llama-3.2-8b-instruct-q8_0.gguf
export COSMON_LLAMA_BENCH_GGUF_24B=~/.local/share/cosmon/models/by-name/mistral-small-24b-instruct-2501-q5_k_m.gguf
export COSMON_LLAMA_BENCH_GGUF_32B=~/.local/share/cosmon/models/by-name/qwen2.5-32b-instruct-q5_k_m.gguf
export COSMON_LLAMA_BENCH_GGUF_CODER_32B=~/.local/share/cosmon/models/by-name/qwen2.5-coder-32b-instruct-q4_k_m.gguf
export COSMON_LLAMA_BENCH_GGUF_70B=~/.local/share/cosmon/models/by-name/llama-3.3-70b-instruct-q4_k_m.gguf

# 2. Rust harness (TTFT + total per call, NDJSON to stdout).
cargo bench -p cosmon-provider --features llama --bench llama_bench \
    | tee docs/benches/llama-v0-acceptance.ndjson

# 3. Upstream cross-validation (decode tok/s — must agree within ±10%).
scripts/llama-bench.sh > docs/benches/llama-v0-acceptance.upstream.md
```

---

## Hardware + build fingerprint

| field                    | value                                                |
|--------------------------|------------------------------------------------------|
| machine                  | MacBook Pro M4 Max — *not observed (SPHardwareDataType unavailable inside cosmon worktree shell)* |
| macOS                    | 26.4.1 (build 25E253)                                |
| llama.cpp SHA (vendored) | `9a532ae4bab1b164052ce60a738f78538b421c66` (tag `b9222`; snapshot copy, no `.git/` lineage — see [VENDOR.md](../../crates/cosmon-llama-sys/vendor/llama.cpp/VENDOR.md)) |
| ggml SHA                 | same snapshot (ggml/ shipped as a subtree of the `b9222` tarball) |
| cosmon revision          | `034c151d1d5e77216f1ab81b9a8848f07d96920e` (merge `feat/task-20260519-a226`) |
| compiler                 | `rustc 1.94.1 (e408947bf 2026-03-25)`                |
| llama-bench (upstream)   | **not installed** on this operator's `PATH` — `scripts/llama-bench.sh` exited 2 (install via `brew install llama.cpp`) |
| run wall-clock           | 2026-05-19 (W8 e6db, this molecule)                  |

---

## Bench plan (target table, from forgemaster's synthesis §S5)

The columns are pinned to the briefing. **H0** = forgemaster's prior;
**Measured** = the cell from the bench output that should land in this
report. **Pass?** = pass-threshold gate.

| Model                              | Quantisation | H0 tok/s | H0 TTFT (4096) | Measured tok/s | Measured TTFT (4096) | Pass threshold                | Pass? |
|------------------------------------|--------------|----------|----------------|----------------|----------------------|-------------------------------|-------|
| Llama-3.2-8B-Instruct *(smoke)*    | Q8_0         | ≥ 55     | ≤ 10s          | *not measured (GGUF absent)* | *not measured (GGUF absent)* | TTFT ≤ 15s AND ≥ 45 tok/s | *—*   |
| Mistral-Small-24B-Instruct-2501    | Q5_K_M       | ≥ 28     | ≤ 55s          | *not measured (GGUF absent)* | *not measured (GGUF absent)* | TTFT ≤ 60s AND ≥ 25 tok/s | *—*   |
| Qwen2.5-32B-Instruct               | Q5_K_M       | ≥ 22     | ≤ 90s          | *not measured (GGUF absent)* | *not measured (GGUF absent)* | TTFT ≤ 90s AND ≥ 20 tok/s | *—*   |
| Qwen2.5-Coder-32B-Instruct         | Q4_K_M       | ≥ 28     | ≤ 70s          | *not measured (GGUF absent)* | *not measured (GGUF absent)* | TTFT ≤ 75s AND ≥ 25 tok/s | *—*   |
| Llama-3.3-70B-Instruct             | Q4_K_M       | ≥ 8      | ≤ 160s         | *not measured (GGUF absent)* | *not measured (GGUF absent)* | TTFT ≤ 160s AND ≥ 7 tok/s | *—*   |

> **Forensic-discipline note (2026-05-19, W8 e6db).** No GGUF fixture
> was present on this operator's disk on the day of this run
> (`~/.local/share/cosmon/models/by-name/` did not exist; none of the
> five `COSMON_LLAMA_BENCH_GGUF_*` env vars were set). The harness
> therefore emitted 15 `missing_fixture` rows (5 models × 3 prompts)
> rather than wall-clock numbers. The cells above are *not* placeholders
> awaiting an estimate — they record the absence of measurement. The
> verdict line at the bottom of this file remains *PENDING* until the
> operator pre-caches at least the three fixtures the briefing names
> (8B smoke + 24B + one of 32B / 70B) and re-runs the bench. See
> an internal chronicle for the loud-chronicle entry.

### Pass criteria per prompt size

These are the **prompt-size** gates the Rust harness enforces. They apply
in addition to the per-model gates above; a row passes only if both
gates are clear.

| Prompt           | Target input tokens | TTFT ceiling | Total wall-clock ceiling |
|------------------|---------------------|--------------|--------------------------|
| haiku            | ~100                | 3 s          | 12 s                     |
| small-code-edit  | ~1 500              | 20 s         | 45 s                     |
| reasoning-chain  | ~4 096              | 90 s         | 180 s                    |

### Hard operator-binding gate

If TTFT > 120s for the 4096-token prompt with the chosen v0 default
model, the harness loop becomes untenable (a 20-call task would cross
40 minutes — *"compute is cheap"* breaks). The Rust harness emits a
`HARD GATE` warning on stderr in that case. Action: drop to a smaller
model (32B → 24B) before escalating to "the harness needs redesign".

---

## Per-prompt measurements

The NDJSON in `llama-v0-acceptance.ndjson` carries every cell. The
tables below extract the columns the verdict line consumes — one table
per prompt size. **PENDING** rows are operator-action items; replace
them with the matching NDJSON row after running the bench.

### haiku — 100 input tokens, 200 output tokens

| Model                              | TTFT (s) | Total (s) | Completion tok | Decode tok/s | Status     |
|------------------------------------|----------|-----------|----------------|--------------|------------|
| Llama-3.2-8B-Instruct              | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Mistral-Small-24B-Instruct-2501    | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Qwen2.5-32B-Instruct               | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Qwen2.5-Coder-32B-Instruct         | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Llama-3.3-70B-Instruct             | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |

### small-code-edit — ~1500 input tokens, 200 output tokens

| Model                              | TTFT (s) | Total (s) | Completion tok | Decode tok/s | Status     |
|------------------------------------|----------|-----------|----------------|--------------|------------|
| Llama-3.2-8B-Instruct              | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Mistral-Small-24B-Instruct-2501    | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Qwen2.5-32B-Instruct               | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Qwen2.5-Coder-32B-Instruct         | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Llama-3.3-70B-Instruct             | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |

### reasoning-chain — ~4096 input tokens, 200 output tokens

| Model                              | TTFT (s) | Total (s) | Completion tok | Decode tok/s | Status     |
|------------------------------------|----------|-----------|----------------|--------------|------------|
| Llama-3.2-8B-Instruct              | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Mistral-Small-24B-Instruct-2501    | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Qwen2.5-32B-Instruct               | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Qwen2.5-Coder-32B-Instruct         | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |
| Llama-3.3-70B-Instruct             | *missing_fixture* | *—*       | *—*            | *—*          | env var unset |

### Upstream cross-validation (decode tok/s, ±10 % gate)

Output of `scripts/llama-bench.sh`, summarised — only the decode tok/s
(`tg` column) is compared against the Rust harness. If any cell
disagrees by more than 10 %, treat it as a regression in either layer
and investigate before declaring v0.

| Model / quant                      | Rust decode tok/s | Upstream `tg` tok/s | Agreement | Verdict |
|------------------------------------|-------------------|---------------------|-----------|---------|
| *not cross-validated (2026-05-19)* | *—*               | *—*                 | *—*       | `llama-bench` upstream not installed; `scripts/llama-bench.sh` exited 2. Re-run after `brew install llama.cpp`. |

---

## Verdict

> **PENDING-operator-fixtures (2026-05-19).** The bench harness ran to
> completion on cosmon revision `034c151d1d5e77216f1ab81b9a8848f07d96920e`
> (rustc 1.94.1, macOS 26.4.1) and emitted 15 `missing_fixture` NDJSON
> rows — *no operator GGUF was on disk and no `COSMON_LLAMA_BENCH_GGUF_*`
> env var was set*. ADR-104's "default model" verdict is therefore the
> single open item gating final acceptance; it is **a data dependency,
> not a code dependency**. To close: the operator pre-caches at least
> the three fixtures the briefing names (8B smoke + 24B + one of 32B /
> 70B), re-runs `cargo bench -p cosmon-provider --features llama
> --bench llama_bench`, and pastes the resulting `ran`-status rows here.

The verdict line is the single sentence consumed by
`task-20260519-6a29` (ADR + chronicle). Choose one of the following
forms, with measured numbers substituted:

- **Default chosen:**
  *"32B Q5_K_M shipped as v0 default — TTFT 47.2 s, decode 22.5 tok/s on
  the reasoning-chain prompt, both within the briefing's gate."*
- **Default + drop:**
  *"24B Q5_K_M shipped as v0 default; 70B Q4_K_M dropped — TTFT 187 s
  exceeds 160 s gate on reasoning-chain prompt."*
- **Failure escalation (rare):**
  *"No model clears the reasoning-chain gate on this hardware — escalate
  to harness-redesign delib."*

The chosen v0 default model name and version is also wired into
`cs ask --provider llama` as the default model when `--model` is
omitted (see [`task-20260519-cfe0`](../../.cosmon/state/) — if the
adapter's hardcoded default differs from this report's verdict, file a
one-line fix commit before merging).

### Reasons for any missing rows

`missing_fixture` rows are acceptable in v0 — the briefing requires
**3 of 5** cells to be measured (8B smoke + 24B + one of 32B / 70B).
Document any skipped row here:

- *Llama-3.3-70B-Instruct Q4_K_M:* not pre-cached by operator — defer
  to v1 if the chosen default already clears the gate.
- *Qwen2.5-Coder-32B-Instruct Q4_K_M:* not pre-cached by operator —
  defer.

---

## NDJSON schema (for reference)

The Rust harness emits one row per (model × prompt) cell to stdout:

```json
{
  "model": "Qwen2.5-32B-Instruct",
  "quant": "Q5_K_M",
  "env_var": "COSMON_LLAMA_BENCH_GGUF_32B",
  "prompt": "reasoning-chain",
  "status": "ran",
  "prompt_tokens": 4123,
  "completion_tokens": 200,
  "ttft_s": 47.2,
  "total_s": 56.1,
  "decode_tok_per_s": 22.5,
  "ttft_pass": true,
  "total_pass": true,
  "decode_pass": true,
  "finished_on_eog": false,
  "ttft_ceiling_s": 90.0,
  "total_ceiling_s": 180.0,
  "ttft_ceiling_per_model_4k_s": 90.0,
  "decode_floor_per_model_tok_s": 20.0,
  "error": null
}
```

`status` is one of `ran` | `missing_fixture` | `error`. `*_pass` fields
are absent for non-`ran` rows.

For the report verdict, the consumer only needs `model`, `prompt`,
`ttft_s`, `total_s`, `decode_tok_per_s`, and the three `_pass` booleans:

```sh
jq -c 'select(.status == "ran") | {model, prompt, ttft_s, total_s,
       decode_tok_per_s, ttft_pass, total_pass, decode_pass}' \
    docs/benches/llama-v0-acceptance.ndjson
```
