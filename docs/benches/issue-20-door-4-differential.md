# Issue #20, door 4 — differential replay of the final bench on two builds

**One harness. Two binaries. Red, then green.**

## What this document answers

The door-4 fix (`73c4b2a`) turned arm C of the container bench from red to
green. But the bench was repaired in the same commit, on two observation
points that the new fail-closed behaviour had removed or moved. The honest
formulation of that day's result was therefore *not* "the same harness, byte
for byte, red then green", but:

> arm C goes from red to green with **two documented instrument repairs**,
> made necessary because the new fail-closed behaviour deletes or relocates
> the old observation points.

There is a strong structural argument that those repairs cannot have
manufactured the green — set out in §5 — but it convinces whoever reads the
code. It does not convince whoever reads only the file. The external
reporter is in the second position.

So this document supplies the measurement that needs no code reading. The
harness is frozen in its **final** state and run against **two builds**,
changing nothing else:

| pass | commit | what it is | required outcome |
|---|---|---|---|
| (a) | `4c41738` | the parent of the fix | arm C **red** |
| (b) | `73c4b2a` | the fix | arm C **green** |

If the repaired harness still finds the defect on the parent, its repairs did
not blunt its ability to detect. If it had gone green on the parent, the
repairs had blinded it and the whole file would have to be reopened. Both
outcomes were informative and neither was prejudged; nothing was adjusted to
obtain the first.

## 1. Result

**The final harness finds the defect on the parent and not on the fix.**

```
(a) 4c41738  VERDICT c: NOT PROVEN — an ONBOARDING SCREEN is still certified alive:
             cs tackle exited 0 over it, the session probe-container-startup-doors-a20f
             is still up, molecule=running (the pre-fix pathology, unchanged)

(b) 73c4b2a  VERDICT c: PROVEN — cs tackle REFUSED the blocking onboarding pane:
             rc=1, stderr quotes the screen it refused,
             tmux session probe-container-startup-doors-5fae gone, molecule=pending
```

Arms A, B, D, E returned the same verdict on both builds, which is what makes
arm C's flip attributable to door 4 rather than to the run.

| arm | (a) parent `4c41738` | (b) fix `73c4b2a` |
|---|---|---|
| A — door 3, no credential | REFUSED, rc=1 | REFUSED, rc=1 |
| B — scenario 1, worktree ownership | PROVEN, owner 10001, rc=0 | PROVEN, owner 10001, rc=1 |
| **C — scenario 2, virgin config** | **NOT PROVEN** | **PROVEN** |
| D — scenario 2, onboarded config | PROVEN (composer) | PROVEN (composer) |
| E1 — `claude` direct, no credential | composer, `Not logged in` | composer, `Not logged in` |
| E2 — `claude` direct, placeholder | composer, `Not logged in` | composer, `Not logged in` |

Arm B's exit status differs (0 vs 1) and its worktree survives on the parent
and is rolled back on the fix. That is not noise: it is the same fail-closed
teardown that forced instrument repair M3b, showing up as a second, independent
trace of the fix being present. Arm B's *verdict* — the reported provisioning
refusal absent, worktree owned by 10001 — is unchanged.

## 2. Arm C's four post-conditions, measured one by one

Arm C does not grade a pane; since the fix a correct refusal leaves no pane.
It asserts the four ways a refusal is observable from outside the process.

| # | post-condition | (a) parent | (b) fix |
|---|---|---|---|
| 1 | `cs tackle` exits non-zero | **no** — `rc=0` | **yes** — `rc=1` |
| 2 | stderr quotes the blocking onboarding pane, anchored on `Pane showed:` | **no** | **yes** |
| 3 | the tmux session is torn down | **no** — session still up | **yes** — `no server running on /tmp/tmux-10001/arm-c-6eb6` |
| 4 | the molecule is not left `running` | **no** — `status=running` | **yes** — `status=pending` |

All four fail on the parent. All four pass on the fix. Post-conditions 1, 3
and 4 have not been touched since `5587114`, before the fix existed.

### What the parent actually did (raw)

```
--- cs tackle (no --permission-mode: default bypassPermissions) ---
 Tackling probe-container-startup-doors-a20f (task-work)
  molecule: task-20260725-a20f
  branch:   feat/task-20260725-a20f
  worktree: /home/cosmon-worker/arm-c/.worktrees/task-20260725-a20f
  session:  probe-container-startup-doors-a20f
  attach:   tmux -L arm-c-6eb6 attach -t probe-container-startup-doors-a20f
ARM_C_TACKLE_RC=0
--- ARM_C_PANE_AT_RETURN (socket=arm-c-6eb6 session=probe-container-startup-doors-a20f) ---
Welcome to Claude Code v2.1.220
 ...
 Select login method:
 ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise
```

Exit 0, a success banner, and a pane parked on a menu waiting for a human.
That is issue #20's door 4, verbatim, found by the **final** harness.

### What the fix did (raw)

```
cs: cs tackle: claude session probe-container-startup-doors-5fae never reached a
work-accepting state within 30s (status=loading). The pane is alive but is not a
composer — typically an onboarding or consent screen waiting for a human ...
Pane showed: Welcome to Claude Code v2.1.220 | Let's get started. | … |
ARM_C_TACKLE_RC=1
--- ARM_C_PANE_AT_RETURN (socket=arm-c-6eb6 session=probe-container-startup-doors-5fae) ---
no server running on /tmp/tmux-10001/arm-c-6eb6
```

Exit 1, the refused screen named in the refusal, and no carcass left behind.

## 3. Why the comparison is legitimate — the frozen harness

The final harness lives on a branch **newer** than the parent commit. A plain
`git checkout 4c41738` would therefore silently restore the *old* harness, and
the comparison would vary two things while believing it varied one — a false
result that is invisible after the fact.

The driver forecloses that. It checks each commit out into a detached
worktree, then force-copies the final `in-container-bench.sh` and `Dockerfile`
over it, and verifies the SHA-256 **twice, before each pass**, treating a
mismatch as fatal rather than repairable:

1. on disk, after the copy into the build context;
2. **read back out of the built image**, from
   `/usr/local/bin/container-worker-doors-bench` — because what a `COPY` line,
   a stale layer or a `.dockerignore` actually delivered is a different fact
   from what sat in the context directory.

```
harness_path         docker/container-worker-doors/in-container-bench.sh
harness_sha256       c9808df181dfaa5a25c27aac8513faa75c9d055d33e2585dc5f5e320b3ac12fc
harness_bytes        35565
dockerfile_sha256    6c76e2d1d945276afd63f5081dfff571f13a3b8f00bc7b2087a4398cc40d3913
```

Both passes, both readings, both runs: **the same value**. The parent's
worktree reports ` M docker/container-worker-doors/in-container-bench.sh` —
visible proof that the old harness *was* restored by the checkout and *was*
overwritten before the build.

## 4. Provenance — which build is which

The bench's own provenance block greps three fix-only strings out of the
shipped `cs`. All three are **already present at `4c41738`**: they came from
fixes 1–3, which landed before the parent. They prove the binary is not the
v0.3.0 tag; they cannot separate the two builds here.

The discriminant for door 4 is `COSMON_READINESS_TRACE`, the env var of
`cosmon_transport::readiness_trace` — a module introduced by `73c4b2a`.

| marker | (a) parent | (b) fix |
|---|---|---|
| `no usable Claude Code credential` | PRESENT | PRESENT |
| `hasTrustDialogAccepted` | PRESENT | PRESENT |
| `awaiting-human` | PRESENT | PRESENT |
| **`COSMON_READINESS_TRACE`** | **ABSENT** | **PRESENT** |
| `sha256(/usr/local/bin/cs)` | `0a04dada9acb590d…` | `02fa39d89e9bd052…` |

That fourth grep runs in the **driver**, against the image, and is deliberately
*not* in the bench script: adding it would change the harness and destroy the
very property this replay exists to establish.

It shows up a second time, from inside the harness, without being asked to.
The bench prints the readiness trace when there is one; on the parent it
printed `readiness trace: EMPTY or absent at …/readiness-trace-arm-c.jsonl (the
loop wrote nothing)`, because the binary has no such module. On the fix the
trace is populated.

## 5. The structural argument, which stands on its own

Independently of any measurement: the edited assertion (M3a) describes **the
content of a refusal**.

Before the fix there was *no refusal*. `cs tackle` exited 0, printed a success
banner, left the session up and the molecule `running`, and never emitted the
string `Pane showed:` at all. An assertion about the wording of a message that
is never produced cannot make the old behaviour pass.

The measurement in §1–§2 and this argument are independent, and each stands
alone. §2 also shows the argument is not load-bearing by itself: three of the
four post-conditions predate the fix entirely and each fails on the parent.

The full instrument history, with the same question asked of every edit, is in
[`../../docker/container-worker-doors/HARNESS-CHANGELOG.md`](../../docker/container-worker-doors/HARNESS-CHANGELOG.md).

## 6. Pinned environment — identical across both passes

Recorded once and shared by both passes of a run.

```
host                Darwin 25.5.0, xnu-12377.121.10~1, arm64 (T6041)
docker_context      desktop-linux      (the reporter's engine: Docker Desktop,
                                        macOS arm64, LinuxKit VM)
docker_client       27.3.1
docker_server       27.3.1
engine_os           Docker Desktop
engine_arch         aarch64
engine_kernel       6.10.11-linuxkit
container_uname     Linux 6.10.11-linuxkit #1 SMP Thu Oct  3 10:17:28 UTC 2024 aarch64
builder image       docker.io/library/rust:1.88-bookworm
                    @sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0
runtime image       docker.io/library/rust:1-bookworm
                    @sha256:77fac8b98f9f46062bb680b6d25d5bcaabfc400143952ebc572e924bcbedc3fa
claude              2.1.220 (Claude Code)   — identical in both images
cs --version        cs 0.3.0                — identical in both; the version
                                              string does NOT distinguish them,
                                              which is why §4 exists
user.max_user_namespaces = 31321; unprivileged userns creation SUCCEEDS as uid 10001
```

Bench configuration, identical in both passes:

```
arm A   /work/arm-a                     root, COSMON_WORKER_UID=10001, no credential
arm B   /work/arm-b                     root, COSMON_WORKER_UID=10001,
        CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-arm-b, HOME=/home/cosmon-worker
arm C   /home/cosmon-worker/arm-c       setpriv uid 10001, CLAUDE_CONFIG_DIR
        =/home/cosmon-worker/.claude-arm-c, VIRGIN (no .claude.json)
arm D   /home/cosmon-worker/arm-d       as C but .claude.json seeded
        {"hasCompletedOnboarding":true,"projects":{}}
arm E   /home/cosmon-worker/ws-e1|e2    claude driven directly, both consent gates
        pre-granted by hand
trace   COSMON_READINESS_TRACE=/home/cosmon-worker/readiness-trace-arm-<c|d>.jsonl
timeouts  arm A 180s, arms B/C/D 240s; 25s settle before any pane capture
```

### The fake credential

Every arm that needs door 3 open mints, **inside the container**, a
`.credentials.json` whose token fields are the literal string:

```
PLACEHOLDER-NOT-A-CREDENTIAL-cosmon-bench-issue-20
```

with `expiresAt: 0`. It authenticates nothing. Door 3 `stat`s the file and
never reads it. No real secret is created, requested, copied, read or logged
anywhere in this pipeline, and no host credential is mounted.

## 7. What this document does NOT claim

* **Nothing about `IS_SANDBOX`.** That axis was not varied and no conclusion
  is drawn about it.
* **No claim of a code change.** This molecule measured and documented. No
  Rust was modified to justify a deliverable; the repository gains a driver,
  a register, and this report.
* **`cs --version` is 0.3.0 in both images** and is useless as a
  discriminant. Use §4.

## 8. Reproducing it

```sh
open -ga Docker                                  # the reporter's engine
scripts/container-worker-doors-differential.sh
```

Roughly 10 minutes on a cold cargo cache per pass, a few minutes warm. The
driver prints the harness hash before each pass and aborts on a mismatch. It
removes only the two image tags it created and the two detached worktrees it
made; nothing pre-existing of yours is a candidate for deletion.

Raw, unsummarised outputs of both passes of both runs are in
[`issue-20-door-4-differential/`](issue-20-door-4-differential/). Only ANSI
colour escapes were stripped, so the files read as text; no line was removed,
reordered or condensed. The `.ansi.log` originals are kept beside them.

## 9. Replication

The whole differential was run three times, end to end, on the same host and
engine.

| | run 1 | run 2 | run 3 |
|---|---|---|---|
| started (UTC) | 2026-07-25T22:46Z | 2026-07-25T23:03Z | 2026-07-25T23:15Z |
| harness sha256, on disk | `c9808df1…` | `c9808df1…` | `c9808df1…` |
| harness sha256, in the image | *(not read)* | `c9808df1…` | `c9808df1…` |
| `sha256(cs)` parent | `0a04dada9acb590d…` | `0a04dada9acb590d…` | `0a04dada9acb590d…` |
| `sha256(cs)` fix | `02fa39d89e9bd052…` | `02fa39d89e9bd052…` | `02fa39d89e9bd052…` |
| `COSMON_READINESS_TRACE` parent / fix | ABSENT / PRESENT | ABSENT / PRESENT | ABSENT / PRESENT |
| **arm C on parent** | NOT PROVEN | NOT PROVEN | NOT PROVEN |
| **arm C on fix** | PROVEN | PROVEN | PROVEN |
| arms A, B, D, E | identical on both heads | identical | identical |

The two `cs` binaries are bit-identical across all three runs, so runs 2 and 3
replicate the *measurement*, not the build.

Run 1 was made with the first version of the driver, which verified the harness
hash on disk only; runs 2 and 3 added the read-back from inside the image and
the base-image digest capture. All readings agree, and the run-1 outputs are
published unchanged rather than re-issued under the newer driver.
