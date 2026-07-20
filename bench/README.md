# cosmon regression bench

A reproducible bench that **reproduces and measures** the six issues an external
tester reported against `cosmon-cli v0.2.1`, and re-measures them against the
**fixed tree**. It turns a prose bug report into a red/green, machine-readable
status report so the fix work is measurable (before/after delta).

The bench **never modifies cosmon source**. It materialises the tree at
`COSMON_TAG` via `git archive` and tests it as-is. `COSMON_TAG` defaults to
**`HEAD`** (the fixed tree); set `COSMON_TAG=v0.2.1` to re-measure the original
baseline. The runtime-decisive probes (#1/#3/#4) use a prebuilt `cs` binary at
`CS_BIN` (default `target/release/cs`, overridable for A/B runs).

Verdict semantics on the fixed tree: **GREEN** = the reported defect no longer
reproduces (a measured fix); **RED** = it still reproduces; **INCONCLUSIVE** =
the discriminating step could not run here (never a silent pass).

## Quick start

```sh
bench/run.sh            # all six probes + aggregate -> bench/out/report.json
bench/run.sh --static   # skip docker builds (fast; docker halves -> INCONCLUSIVE)
bench/smoke-dispatch.sh # one real headless probe -> $MOLECULE_DIR/dispatch-output/
bench/judge/run-judge.sh  # merge the LLM-as-judge second opinion column
```

Prerequisites:

- `git`, `jq`, `bash`, `rg` (ripgrep) — required (static probes).
- `docker` — required for probe #2's build discrimination and the runtime
  reproduction halves of #1/#3/#4/#6.
- `ollama` + a tiny model (e.g. `qwen2.5:0.5b`) — for probe #4's decisive half.
- `cosmon-remote` authed, or `COSMON_JUDGE_CMD` — for a real LLM-as-judge run.

Where a probe needs an external binary that cannot run headless (a fully authed
Claude Code session), it degrades to asserting the argv/spawn signature and
marks that portion **INCONCLUSIVE** with an explicit note — never a silent pass.

## The six probes

| id | issue | decisive half needs |
|----|-------|---------------------|
| `issue-1-cs-verify`     | `cs verify` on a fresh molecule (tester's `emitter_kind` story is contradicted in source) | built `cs` |
| `issue-2-build-deps`    | from-source Linux build needs `pkg-config` + `libdbus-1-dev` | docker |
| `issue-3-dag-orphan`    | `cs run` stalls the DAG on a dead worker; completed nodes not re-run | built `cs` |
| `issue-4-local-ollama`  | local adapter books "completed" on Ok with no output check | built `cs` + ollama |
| `issue-5-paper-cuts`    | mangled license URL / `/srv/cosmon` persona paths / `git diff` dump | nothing (static) |
| `issue-6-claude-adapter`| TUI + `send-keys` + `bypassPermissions` argv, **not** headless `--prompt` | argv static; runtime needs authed claude |

## Report schema (`bench/out/report.json`)

```json
{
  "unit_under_test": "v0.2.1",
  "probe_count": 6,
  "populated": 6,
  "verdict_tally": { "RED": 0, "INCONCLUSIVE": 0 },
  "rows": [
    {
      "id": "issue-5-paper-cuts",
      "name": "...",
      "adapter": "static",
      "verdict": "RED|GREEN|INCONCLUSIVE",
      "captured_signature": "...",
      "evidence_path": "out/evidence/issue-5-paper-cuts.txt",
      "judge_verdict": "RED|GREEN|INCONCLUSIVE|PENDING",
      "note": "..."
    }
  ]
}
```

Verdict semantics:

- **RED** — the reported defect reproduced (bad behaviour observed).
- **GREEN** — the reported defect did not reproduce on this tree.
- **INCONCLUSIVE** — the discriminating step could not run headless here. This
  is *not* a pass; the note says exactly what was missing.

## LLM-as-judge (null context)

`bench/judge/` hands a **fresh** cosmon worker the same six-issue mission
(`JUDGE_MISSION.md`) against a pristine `v0.2.1` tree, with no access to this
molecule's context and no access to the tester's report. Its independent
per-issue verdict becomes the `judge_verdict` column — a second opinion scored
without the bench's priors, so a human can re-run the whole analysis
reproducibly and the before/after fix delta is judged by something neutral.

## Layout

```
bench/
  run.sh              full production path (materialise -> probes -> aggregate)
  smoke-dispatch.sh   one real headless dispatch -> molecule dispatch-output
  aggregate.sh        probes/*.json -> report.json + report.md
  Dockerfile          Linux/glibc image, builds v0.2.1 WITH deps (probe #2 +)
  Dockerfile.nodeps   same WITHOUT pkg-config/libdbus (probe #2 -)
  lib/common.sh       producer core: checkout, evidence, emit_probe
  probes/             one script per issue
  judge/              LLM-as-judge harness + mission
  out/                generated reports + evidence (gitignored)
```

Phase 0 deliverable: **reproduce + measure**. No fix to any of the six issues
lives here.
