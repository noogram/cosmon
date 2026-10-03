# Harness evaluation: identical tasks, measured routing

This guide covers `tests/harness-eval/`: a frozen task corpus, a runner and a
validator for comparing the in-process arms (`openai`, `anthropic`, the `local`
floor) with the established subprocess route. It decides nothing by itself. The
runner never changes a default; an operator reads the report.

## What is frozen

`manifest.toml` lists six tasks, each with a pinned input digest and an
acceptance script that lives outside the worker's workspace:

| Task | Property | Needs |
|---|---|---|
| `text-output` | declared text output | none |
| `bounded-edit` | edit one file, external tests decide | none |
| `gate-tail` | several artifacts, then a gate | `shell` |
| `peer-evidence` | answer only reachable through a peer message | `peer_messaging` |
| `large-context` | answer buried in a 400-line input | `large_context` |
| `interrupted-effects` | each side effect applied exactly once across an interruption | `shell`, `resume` |

Tasks, digests and acceptance scripts are identical for every arm. A task is not
reworded or its expected output changed after a failure; add a new id and bump
`corpus_version`. `validate.py --check-corpus` recomputes every digest and
`run.py` refuses to start on a drifted fixture. An arm lacking a required
capability gets `inapplicable`, never `failed`.

## What counts as success

A run is `accepted` only when the independent acceptance script ran and passed,
every required artifact exists, the gate passed where the task has one, and the
input digest matches the corpus. The exit code and the worker's own claim of
success are recorded, never sufficient. Other outcomes are `rejected`,
`inapplicable` and `invalid` (the record contradicts itself, for example a usage
row counted twice, or a live arm that was not configured). Every planned run is
kept in the report with its outcome.

Per run the record keeps the revision, a digest of the arm configuration, the
requested and observed model, allowed tools, input digest, retries, interventions,
wall time, per-request usage and cost. Cost is `complete` only when every request
has a known price and the total equals their sum; an unknown price stays `null`.
Task success and interventions are reported as separate columns. Live and mocked
runs are separate sections of the summary.

## Commands

```text
python3 tests/harness-eval/validate.py --self-test       # evaluator self-test
python3 tests/harness-eval/validate.py --check-corpus    # pinned digests
python3 tests/harness-eval/run.py --fixtures-witness     # input fails, reference solution passes
python3 tests/harness-eval/run.py --self-test            # mock arms end to end
python3 tests/harness-eval/run.py --out DIR              # mocked arms, writes DIR/report.json
python3 tests/harness-eval/validate.py --report DIR/report.json
```

The first four run inside `just quick`. They need Python 3.11+ or the `tomli`
package. The mocked arms are deterministic stand-ins: `stub-solver` applies the
reference solution, `stub-noop` exits 0 claiming success with no deliverable,
`stub-replayer` replays side effects and double-counts usage, `stub-shell-free`
lacks `shell` and `resume`. They run through the same path as a live arm, so the
evaluator is exercised on exactly the failures it exists to catch. The
`stub-noop` arm is the case a naive exit-code evaluator scores as a success.

## Running the live comparison

Not done by this change. The live arms (`direct-chat`, `direct-messages`,
`local-floor`, `subprocess-baseline`) have an empty `command`; the evaluation
molecule fills each in, and `run.py --live --spend-cap-usd N --trials K` is
required to run them. Without both, a live arm records `invalid` and no process
starts. Once the cap is reached the remaining planned live runs are recorded as
`invalid` with the reason rather than dropped. Before comparing, run the
baseline arm alone and keep its raw runs under the evaluation molecule's
directory, not the tracked fixtures.

Arm order in trial `t` is the arm list rotated left by `t`; tasks keep manifest
order. The recommendation section reports accepted counts with 95% Wilson
intervals over applicable runs, states its uncertainty, and has
`switch_default: false`. A routing change needs the operator's trial count, spend
cap and adoption threshold (plan questions Q9). A performance or root-cause claim
drawn from the results still needs `diagnosis-discipline.md` and an independent
provider-family refutation.

## Limits

- The corpus is small and synthetic. It measures acceptance on six task shapes,
  not general coding quality.
- Mocked evidence validates the evaluator only.
- Interruption is part of the `interrupted-effects` task statement; the driver
  for a live arm must perform the interruption and the resume. The mock arms
  simulate the outcome.
- Unbounded in-memory shell and response buffering is not measured here; track
  it separately if long runs expose it.
