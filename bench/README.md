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

### Which docker engine (corrected 2026-07-27)

Two different rules, on purpose:

- The **container benches** under `scripts/` —
  `container-worker-doors-bench.sh`, `container-real-mission-bench.sh`,
  `container-worker-doors-differential.sh` — pin the dedicated colima profile
  `cosmon-bench`, resolved in one place by `scripts/lib/bench-engine.sh`. They
  used to pin `desktop-linux` under a header claiming Docker Desktop was the
  external tester's engine and colima "NOT faithful". He corrected his own
  description on 2026-07-27 (Colima / Ubuntu 24.04.4 LTS / aarch64), and both
  engines were then measured rather than re-guessed: on `desktop-linux`,
  `unshare` as a non-root uid succeeds and a bind mount honours `chown`, so
  **neither** of his two standing findings can reproduce there.
  → [`docs/benches/engine-fidelity-2026-07-27.md`](../docs/benches/engine-fidelity-2026-07-27.md)

  If that engine is down they exit **2 = INCONCLUSIVE** with the exact
  `colima start` line, and never fall back to another context — same verdict
  semantics as everything else here, applied to the engine itself.

- The **six probes** in `bench/probes/` keep using whatever docker context is
  current, and degrade to INCONCLUSIVE when there is none. This is deliberate,
  not an oversight: the only docker-dependent probe, `issue-2-build-deps`, asks
  whether a from-source Linux build needs `pkg-config` + `libdbus-1-dev`. That
  answer is a property of the *image's* apt state, not of the host kernel's
  user-namespace or mount posture, so pinning a VM there would cost a boot and
  buy no fidelity. If a probe ever grows a kernel- or mount-sensitive half, it
  moves onto `scripts/lib/bench-engine.sh` with the others.

Where a probe needs an external binary that cannot run headless (a fully authed
Claude Code session), it degrades to asserting the argv/spawn signature and
marks that portion **INCONCLUSIVE** with an explicit note — never a silent pass.

### The real-mission arm — how far a machine may actually walk

The sentence above is a real limit, and for a long time it was also a blind
spot: the container benches proved the four startup doors of issue #20 *open*,
but every arm of them stops in front of a file literally named
`PLACEHOLDER-NOT-A-CREDENTIAL`, so nothing had ever been observed walking down
the corridor behind those doors.

`scripts/container-real-mission-bench.sh` closes that gap up to the one step a
machine must not take. It builds the tester's environment
(`docker/container-real-mission/Dockerfile`), installs `cs` from the current
tree, and drives a **real** molecule through the **real**
`cs tackle --adapter claude` path — no placeholder minted, nothing doubled. It
then necessarily halts at door 3 and asserts the refusal's post-conditions,
capturing the gate's own words verbatim into `mission-record.json`.

**A refusal for the expected reason is the measured outcome, not a failure.**
An exit-0 would be the alarming result.

The remaining step, the login, belongs to a human. The harness prints the exact
command; the two ways to provision that credential and their costs are set out
in `docs/guides/claude-worker-in-a-container.md`.

#### It grades against the world it is in, not against one world

The first version of this arm only knew the world with no credential in it, so
it treated a refusal as the pass and a *successful* dispatch as a finding. Once
the human completed the login, it reported failure over a run that worked.

The in-container grader now decides which world it is in by `stat()`ing the
credentials path — never by opening it, the secret discipline is unchanged —
and grades accordingly:

| world | discriminator | expected outcome (exit `0`) |
|---|---|---|
| no credential | `$CLAUDE_CONFIG_DIR/.credentials.json` absent | `REFUSED-AT-CREDENTIAL-GATE` — the gate held and named the credential |
| credential present | that file present | `SPAWNED-LIVE-WORKER` — tackle exited `0`, the named tmux session answers `has-session`, and the molecule is no longer `pending` |

The second row is asserted **positively**. A zero exit code proves only that a
process exited; it is never taken as evidence that a worker exists.

Same verdict semantics as above in both worlds: exit `0` the expected outcome,
exit `1` a finding, exit `2` INCONCLUSIVE with the reason printed. **Neither
world may pass silently when its discriminating step could not run** — a
missing docker engine, a molecule that never nucleated, or a `0` tackle that
named no session to probe are all exit `2`, never green.

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
