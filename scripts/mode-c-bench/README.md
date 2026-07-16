# mode-C tool-call falsification bench

**Pre-registered** falsification bench for mode-C long-tool-call survival —
task-20260707-5fe6, brief in `delib-20260707-df9b` `outcomes.md` §M-BENCH.

The predicate is fixed **before** any run (`lib.sh`). That is the whole point:
a bench you tune after seeing the result cannot falsify anything.

## The failure it falsifies

Mode C runs cosmon's `OpenAIProvider` in-process against ollama's
`/v1/chat/completions`. When a local model emits a tool call whose arguments
carry an entire script as one JSON string, ollama's **server-side** tool-call
parser can reject it with **HTTP 500** (`... error parsing tool call ...`).
Before task-20260707-4991 that 500 was fatal — the worker died at role 1/9 of
the academy physics-intern mission (`task-20260707-c253`,
`delib-20260707-50f5`) with zero artefacts. M1 (`delib-20260707-df9b`) added
typed retry classification + disk-evaluable events.

The 500 is a **joint property of model-output-shape × server-parser**, not of
the prompt alone. So the model + endpoint are **pinned** (`BENCH_MODEL=gpt-oss:120b`,
`BENCH_OLLAMA=http://127.0.0.1:11436`, a tunnel to the g5 box hosting the 120B
zoo — **not** local ollama). An absent pin is `INCONCLUSIVE-UNAVAILABLE`, never
silently substituted.

## Verdicts (never a bare pass/fail)

| Verdict        | Condition                                                        |
|----------------|-----------------------------------------------------------------|
| `RECOVERED`    | 500 **fired** AND loop **survived** AND artefacts               |
| `DIED`         | 500 **fired**, recovery attempted, worker Stuck, **zero** artefacts |
| `INCONCLUSIVE` | 500 **never fired** — proves nothing                            |
| `AMBIGUOUS`    | fired, but survival/death signals disagree — never upgraded to PASS |

The `fired >= 1` clause is load-bearing: it rejects a **lucky run** where the
model self-chunks from the first turn and never exercises the recovery path.

### On-disk markers (no tmux archaeology)

- **FIRED**  = `grep -Ec 'tool_parse_reinject' <mol>/events.jsonl`
  (the typed `AdapterLivenessProbed{Retried,"tool_parse_reinject"}` row M1
  writes on each in-place recovery)
- **DEATH**  = `grep -Ec 'tool_call_parse|SF-1 http|SF-1 server_error' <mol>/events.jsonl`
- **SURVIVAL** = `cs observe <id> --json | jq -r .state == "completed"` **and**
  a non-empty `synthesis.md` / `responses/` / `frame.md`

### Aggregate predicate (N=5)

```
PASS                 iff  >= 1 RECOVERED  AND  0 DIED  AND  0 AMBIGUOUS
PASS-WITH-AMBIGUITY  iff  >= 1 RECOVERED  AND  0 DIED  AND  >= 1 AMBIGUOUS
FAIL                 iff  >= 1 DIED
INCONCLUSIVE         otherwise  (all-inconclusive => provocation too weak;
                                 escalate it, do NOT declare victory)
```

## Files

| File                  | Role                                                        |
|-----------------------|-------------------------------------------------------------|
| `lib.sh`              | Shared substrate — pinned provocation, on-disk markers, the 4-verdict `classify_verdict`, the N-run `batch_predicate`. **Single source of truth for the shell side.** |
| `classify.sh`         | Classify one molecule dir (`--json`), or `--selftest` (offline, deterministic, fixture-driven). |
| `run-bench.sh`        | Live N-run replay through cosmon's real mode-C worker; classifies each run + aggregates. |
| `negative-control.sh` | Proof of power: replays the committed pinning test across the fix boundary (`e48d9a0de` → HEAD) and demands **RED → GREEN**. |
| `da-stream-ab.sh`     | The **D-A experiment**: `stream:false` vs `stream:true` of the identical long-tool-call payload against the identical ollama binary — the measurement that gates M2. |
| `fixtures/`           | Sealed `events.jsonl` samples, one per verdict, driving `classify.sh --selftest`. |
| `provocation/`        | The pinned academy role-1/9 mission (`anharmonic-mission.md`) + its `sha256` seal. |
| (Rust) `crates/cosmon-provider/tests/mode_c_falsification_bench.rs` | The **fast deterministic discriminator** — proves the predicate rejects a lucky run and a death, in-binary, in `cargo test`, without a live 500. Markers + verdict table mirror `lib.sh`. |

## Running

```bash
# Fast deterministic discriminating-power proof (no ollama, no cs):
cargo test -p cosmon-provider --test mode_c_falsification_bench
scripts/mode-c-bench/classify.sh --selftest

# Slow deterministic proof of power (two cargo builds across the fix boundary):
scripts/mode-c-bench/negative-control.sh

# Live replay against the pinned model+endpoint (5 runs):
BENCH_N=5 scripts/mode-c-bench/run-bench.sh

# The D-A stream A/B measurement (+ optional local cross-model probe):
scripts/mode-c-bench/da-stream-ab.sh
scripts/mode-c-bench/da-stream-ab.sh --local-probe
```

`BENCH_MODEL` / `BENCH_OLLAMA` / `BENCH_N` override the pins. Without the pinned
model reachable, `run-bench.sh` and `da-stream-ab.sh` print
`INCONCLUSIVE-UNAVAILABLE` and defer to the two deterministic proofs above —
the honest result, not a green light.

## Why the deterministic proofs carry the power

The live 500 needs the *exact* `gpt-oss:120b` × ollama-build pair whose parser
rejects the whole-script call. On a box without that pin the live replay is
correctly `INCONCLUSIVE-UNAVAILABLE`. So the proof-of-power lives in two
deterministic places: the **fast** in-binary Rust discriminator (same mock 500
stimulus flips `RECOVERED → DIED` on whether the server recovers; the
`INCONCLUSIVE` arm refuses to reward a run that never fired), and the **slow**
`negative-control.sh` (the committed pinning test flips RED → GREEN across the
`643c6ae7d` recovery boundary).

See the molecule's `bench-report.md` for the measured results on this box —
including the D-A stream A/B outcome and the availability status of the pin.
