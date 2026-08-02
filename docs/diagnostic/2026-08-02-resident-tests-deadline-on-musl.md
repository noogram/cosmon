# Diagnostic — six `cosmon-runtime` resident tests die on `Deadline` under a musl container

**Date:** 2026-08-02 · **Molecule:** `task-20260802-04be` · **Issue:** #37
(family B of #34) · **Image:**
`rust:1.97-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900`,
non-root uid 10001, `bash` + `tmux` + `procps-ng` installed.

## Verdict: the image has no `python3`, so the stub `cs` could never exec

The six tests drive `RuntimeLoop` against a stub `cs` written in Python. The
pinned Alpine image ships no Python interpreter at all. Every spawn of the
stub therefore returned `ENOENT`, the loop logged `ensemble-read-failed` on
every tick, made no decisions for 60 s, and exited `Deadline`.

Nothing musl-specific is involved. Neither signals, nor `/proc`, nor process
reaping, nor container timing, nor the fixture's model of `cs`.

The affected tests, and the only four test files in the crate that use a
Python stub, are the same set:

| Test file | Tests | Stub language |
|---|---|---|
| `phantom_running_reap.rs` | 1 | Python |
| `resident_config_drift_halt.rs` | 2 | Python |
| `resident_drain_dag.rs` | 1 | Python |
| `sigint_race_suppresses_spurious_error.rs` | 2 | Python |
| `resident_recheck_skip_retries.rs` (**passes** on the image) | 2 | POSIX `sh` |

That last row is the control: the one resident test whose stub is `sh` is also
the one resident test that survives the image.

## Measurements

**1. The interpreter is absent from the pinned image.**

```console
$ docker run --rm --entrypoint /bin/sh $IMG -c 'command -v python3; /usr/bin/env python3 -c "print(1)"'
env: can't execute 'python3': No such file or directory   # rc=127
```

`/usr/bin/python3`, `/opt/homebrew/bin/python3`, `/usr/local/bin/python3` and
`pyenv` are all absent too — that is, every branch of the test helper's
resolution ladder, including its `/usr/bin/env python3` last resort.

**2. The blocking predicate, named from an instrumented run.** Reproduced on
**macOS** — not musl, not a container — by removing only the interpreter
(`COSMON_TEST_PYTHON=/nonexistent/python3`), then dumping the loop's decision
trace:

```json
{"action":"launch","decision_basis":"config-seal-sealed", ...}
{"action":"tick","decision_basis":"ensemble-read-failed",
 "error":"cs ensemble failed for : spawn failed: No such file or directory (os error 2)", ...}
{"action":"tick","decision_basis":"ensemble-read-failed", ...}   ← ×~2000, for 60 s
```

```text
expected Drained after reaping the phantom, got Deadline   # byte-identical to #34
```

The predicate is `read_ensemble(&self.config)` returning `Ok`
(`crates/cosmon-runtime/src/resident.rs`). Its `Err` arm traces
`ensemble-read-failed` and `continue`s, so a loop that cannot read the fleet
never reaches the `drained` check, never dispatches, and never reaps — it can
only run out the `max_runtime` budget. Unsatisfiable, exactly as #31 was.

**3. Cross-platform refutation.** Same symptom, same message, on darwin with
an interpreter removed; and on the pinned musl image the six tests pass in
0.02–0.20 s each once `apk add python3` is applied, as uid 10001:

```text
phantom_running_molecule_is_reaped_and_dag_drains ................ ok
config_drift_between_launch_and_dispatch_halts_fail_closed ....... ok
binary_reinstall_does_not_trip_the_seal .......................... ok
three_molecule_dag_drains_under_resident_runtime ................. ok
signal_killed_ensemble_yields_clean_shutdown_trace ............... ok
signal_killed_done_subprocess_yields_clean_shutdown_trace ........ ok
```

A cause that reproduces off-musl and disappears on-musl under a package
install is a missing dependency, not a platform semantic.

## The defect, and what was fixed

The missing package is an environment gap. The *defect* is that the gap was
undiagnosable from its symptom: the helper's last resort returned
`/usr/bin/env python3` **without checking that it runs**, handing the tests an
interpreter that does not exist. Six tests then spent 60 s each waiting on a
predicate that could never hold, and reported `Deadline` — a word that names
neither the missing package nor the seam that broke.

Three changes, all in test scaffolding:

1. `tests/common/mod.rs` — the `/usr/bin/env python3` last resort is now
   *executed* (`env python3 -c ''`) before being returned. When it fails,
   `resolve_python3` panics at setup naming the package to install and the
   override to set. On the pinned image the failure now takes **0.00 s**
   instead of 60 s, and says `apk add python3`.
2. `tests/common/mod.rs` — a shared `dump_trace` helper, wired into all six
   tests, prints the decision trace whenever the run ends on an unexpected
   `ExitReason`. This is what turned the diagnosis into a single run; it costs
   nothing on a green suite.
3. `docs/CONTRIBUTING.md` — the suite's PATH prerequisites (`bash`, `tmux`,
   `python3`, `ps`) and the non-root requirement, per package manager. This is
   the docs line #34 called for.

## Deliberately not done

Rewriting the four Python stubs in POSIX `sh` (as
`resident_recheck_skip_retries.rs` already is), or as a Rust helper binary,
would drop the interpreter dependency outright. It is the more thorough
answer and it is a larger, riskier change: three of the four stubs parse and
rewrite JSON fleet snapshots, which `sh` does badly and which would be
re-implementing the fixture rather than fixing #37. Left as a separate call.

## Lesson

Same shape as #31. A wait is only as good as the predicate under it: when a
loop's input can fail *permanently*, retrying it until a deadline converts a
one-line error into a silent hour. Prefer failing at the seam that broke —
here, at fixture setup, where the missing program has a name.
