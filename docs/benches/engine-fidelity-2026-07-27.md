# Which engine do the container benches run on? — measured, 2026-07-27

For about a year the three container benches pinned `--context desktop-linux`
and each carried a header sentence saying, in as many words, that Docker
Desktop's LinuxKit VM was *the tester's engine* and that "a colima context runs
an Ubuntu kernel with a DIFFERENT user-namespace posture and is **NOT
faithful**".

On 2026-07-27 the external tester corrected his own earlier description,
unprompted, on GitHub issue #20: his bed is **Colima (Lima-based), Ubuntu
24.04.4 LTS, aarch64**. He had said Docker Desktop in the original
reproduction recipe and in several comments afterwards; the correction is his.

So the benches had been pinned *away* from his real engine by a comment written
to keep them faithful to it. This document is the measurement that should have
existed before that sentence was written.

## The correction is not a flip

The old claim had a factual half and a conclusion. The factual half is true: a
colima VM really does run a different kernel from LinuxKit, and its
user-namespace posture really is different. The conclusion drawn from it —
*therefore colima is not faithful* — is what was inverted.

Replacing "colima is not faithful" with "desktop-linux is not faithful" without
probing anything would repeat the original mistake with the sign changed. So
both engines were probed, on this machine, on this date, by
[`scripts/container-engine-posture.sh`](../../scripts/container-engine-posture.sh).
Every line below was run or read; nothing was inferred from a setting.

## What was measured

Host: Darwin 25.5.0, arm64 (`xnu-12377.121.10~1`, RELEASE_ARM64_T6041).
Probe image: `ubuntu:24.04` on both engines, so the userland is held constant
and only the engine varies.

| | `colima-cosmon-bench` | `desktop-linux` |
|---|---|---|
| engine | Colima (Lima), docker 29.2.1 | Docker Desktop, docker 27.3.1 |
| engine OS | Ubuntu 24.04.4 LTS | Docker Desktop |
| kernel | `6.8.0-100-generic` | `6.10.11-linuxkit` |
| arch | aarch64 | aarch64 |
| `/proc/sys/user/max_user_namespaces` | `31515` | `31321` |
| `kernel.unprivileged_userns_clone` | `1` | key absent (**absent is not `0`**) |
| `Seccomp` in `/proc/self/status`, default profile | `2`, `Seccomp_filters: 1` | `0`, `Seccomp_filters: 0` |
| `setpriv --reuid 10001 … unshare -Ur true`, default profile | **BLOCKED** — `unshare failed: Operation not permitted` | **OK** |
| same, `--security-opt seccomp=unconfined` | **OK** | OK |
| bind-mounted host dir: `chown 10001:10001`, then `stat` | rc `0`, owner stayed `0:0` — **silently ignored** | rc `0`, owner became `10001:10001` — **honoured** |

Raw capture: [`engine-fidelity-2026-07-27/posture.txt`](engine-fidelity-2026-07-27/posture.txt).

## What this says

**Docker Desktop reproduces neither of the tester's two standing findings.**
That is the finding, and it is stronger than "the label was wrong". The bench
was not merely running on a mislabelled engine — it was running on the engine
that *cannot see* the failures it was built to reproduce:

- **`unshare` is blocked.** On colima it is, exactly as the tester reported. On
  Docker Desktop it succeeds. A bench pinned to `desktop-linux` would have
  reported this door open on an engine where it was never shut.
- **A bind mount silently ignores `chown`.** On colima the `chown` returns `0`
  and the ownership does not move — the worst shape a failure can take, because
  nothing in the script's own return codes reveals it. On Docker Desktop the
  ownership changes and the whole class of defect is invisible. Three ownership
  defects have now been found in this system; an engine that hides ownership
  failures is the wrong place to look for the fourth.

**The cause of the `unshare` block is attributed, not guessed.** The probe runs
the same command twice, changing exactly one thing. Under the default profile
it is refused; with `--security-opt seccomp=unconfined` it succeeds, and
`/proc/self/status` shows `Seccomp: 2 → 0` across the two runs. The default
seccomp profile is therefore the cause on colima. This matters because our
earlier egress advisory named two sysctls as the cause **without reading them**,
which cost the tester a week and is already chronicled: on colima both sysctls
are in fact permissive (`max_user_namespaces = 31515`,
`unprivileged_userns_clone = 1`) and `unshare` is refused anyway. A setting read
is not a cause measured. Where a probe cannot discriminate, it prints `cause
undetermined` rather than reaching for a plausible key — the same discipline as
`cosmon_core::egress::NetnsBlocker::Undetermined`.

**One thing is deliberately left unexplained.** Docker Desktop 27.3.1 reports
`Seccomp: 0` even under its default profile, where a stock docker engine
normally applies a filter. We did not chase why, and no cause is claimed here —
*cause undetermined*. It does not change the conclusion: whatever the reason,
that engine does not exhibit the block.

## Which engine the benches target, and why

`scripts/container-worker-doors-bench.sh`,
`scripts/container-real-mission-bench.sh` and
`scripts/container-worker-doors-differential.sh` now run on the docker context
`colima-cosmon-bench`, resolved in one place by
[`scripts/lib/bench-engine.sh`](../../scripts/lib/bench-engine.sh).

Two reasons, in this order:

1. **It is the tester's engine shape** — Colima, Ubuntu 24.04.4 LTS, aarch64 —
   as he described it himself after the correction, and both of his standing
   findings reproduce on it (measured above; they do not on the old default).
2. **It is this workshop's platform of choice** anyway. The `forgeron` galaxy's
   README lists colima as a prerequisite, and five colima profiles already
   exist on this machine.

The profile is **dedicated**: `cosmon-bench` belongs to the benches and to
nothing else. It is deliberately *not* `colima-forgeron-build`, nor any
`colima-maqi-*`, nor `default` — each of those carries real work, and a bench
that shares a VM with real work is a bench that will one day break it. The
scripts will not start, stop, prune, or build on any profile but their own.

Start it with the exact shape this capture was taken on:

```sh
colima start --profile cosmon-bench --cpu 4 --memory 8 --disk 60 \
  --vm-type vz --mount-type virtiofs --runtime docker
```

`--vm-type vz --mount-type virtiofs` is load-bearing, not decoration: it is what
puts a virtiofs mount under the bench, which is what makes the silently-ignored
`chown` observable. A qemu profile with sshfs mounts is a different bed and
would quietly change that row of the table.

## The doors bench, run end to end on the new engine

Not a claim that it *should* work — a run. `scripts/container-worker-doors-bench.sh`
was executed on `colima-cosmon-bench` on 2026-07-27 and exited `0` with all
eight arms passing:

```
engine: server=29.2.1 os=Ubuntu 24.04.4 LTS arch=aarch64 kernel=6.8.0-100-generic

VERDICT A:  door 3 REFUSED the dispatch (credential named in the refusal), rc=1
VERDICT B:  PROVEN — worktree owned by 10001, no provisioning refusal, rc=0
VERDICT c:  PROVEN — the pane reached the COMPOSER; doors 1 and 2 passed
VERDICT d:  PROVEN — the pane reached the COMPOSER; doors 1 and 2 passed
VERDICT e1: COMPOSER with the 'Not logged in' footer (the documented shape)
VERDICT e2: COMPOSER with the 'Not logged in' footer (the documented shape)
VERDICT F1: PASSED — the pane reached the COMPOSER, no startup dialog
VERDICT F2: PASSED — the pane reached the COMPOSER, no startup dialog
```

Complete output: [`engine-fidelity-2026-07-27/doors-bench-on-colima.log`](engine-fidelity-2026-07-27/doors-bench-on-colima.log)
(the `.ansi.log` beside it is the same bytes with the colour escapes still in;
no line was removed, reordered or reworded).

Two things in that log are worth reading rather than skimming. The bench's own
arm 0 reports, on this engine, that
`setpriv --reuid 10001 … unshare -Ur true` **fails** — the tester's finding,
reproducing inside the bench that is supposed to be faithful to him, which it
did not do on the old default. And arm B asserts positively that the worktree is
owned by uid 10001, which on this engine means an ownership change that actually
took: the same assertion on a virtiofs bind mount would have been satisfied by a
`chown` that did nothing.

## When the engine is down, the answer is INCONCLUSIVE

Colima was not running on this machine when this work started, and the benches'
old behaviour in that situation — pick another context, or die with a message
about starting Docker Desktop — is how the drift got in. All three benches now
refuse with the exact `colima start` line and exit **2 = INCONCLUSIVE**, the
verdict `bench/README.md` already defines for "the discriminating step could not
run here". Never a pass, never a fallback, and never a failure attributed to the
code under test.

## Reproducing this table

```sh
scripts/container-engine-posture.sh                       # every reachable context
scripts/container-engine-posture.sh colima-cosmon-bench desktop-linux
COSMON_POSTURE_OUT=out.txt scripts/container-engine-posture.sh
```

It creates and removes its own scratch directory, names every container after
its own pid, and `--rm`s them. It **pulls** `ubuntu:24.04` and deliberately does
not remove it: that image may have been on your engine before this ran, and
deleting somebody else's image while tidying up is exactly the accident that
destroyed a live twenty-minute mission on this fleet on 2026-07-27.
