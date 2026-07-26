# Instrument mutation register — the issue-#20 container-doors bench

This file records **every change to the bench harness** since it was
created, and for each one answers the only question that matters to a
reader who was not there:

> *Could this edit have manufactured a green verdict?*

## Why a register, and not just `git log`

The bench measures `cs`. When `cs` changes, the thing being measured moves
— that is the point. But a fail-closed fix also **removes or relocates the
places from which the bench was looking**: a refusal that tears the session
down deletes the pane the bench used to read; a refusal that rolls the
worktree back deletes the file the bench used to `stat`. Repairing the
instrument is then unavoidable, and it is also exactly what a dishonest
report would look like from the outside.

So the repairs are written down here, one by one, with the reason the
observation point moved and the argument that the repair cannot fabricate a
pass. Anyone reading only this directory can audit the instrument's history
without reading a line of Rust.

**Rule — this file is part of the harness.** Any future change to
`in-container-bench.sh` (or to the `Dockerfile` it runs in, or to
`../../scripts/container-worker-doors-bench.sh`) MUST add a row here in the
same commit. A harness edit with no register entry is an unreviewed edit.

## The independent check that does not rely on this file

Trust in a register is still trust. The differential replay
(`scripts/container-worker-doors-differential.sh`) removes the need for it:
it takes the harness in its **final** state — one file, one SHA-256, both
printed by the driver — and runs it against two builds of `cs`, changing
nothing but the commit under test.

* `4c41738` — the parent of the door-4 fix → arm C must be **red**
* `73c4b2a` — the door-4 fix → arm C must be **green**

If the repaired harness still finds the defect on the parent, the repairs
did not blunt it, and no reader has to take the arguments below on faith.
Report: `docs/benches/issue-20-door-4-differential.md`.

---

## Mutations

### M0 — 2026-07-25 · `1568a50` · creation

`test(container): replay issue #20's two scenarios on the fixed branch`

Created `Dockerfile`, `in-container-bench.sh`, and the host driver. Arms A
(door 3, no credential), B (scenario 1, worktree ownership), C and D
(scenario 2, virgin vs onboarded config dir).

*Fabrication risk:* n/a — nothing existed before it.

### M1 — 2026-07-25 · `e33351d` · arm C could see the door but not name it

**Changed.** The scenario-2 pane classifier gained a branch for
`Select login method`, and a new arm E was added that drives `claude`
directly, with cosmon out of the picture (E1 = no credential, E2 =
placeholder credential).

**Why.** The arm that *found* the fourth door reported it as
`INCONCLUSIVE`: the harness could see the pane and had no word for it. A
finding that lands in the "unrecognised" bucket is a finding that will be
lost on the next run.

**Why it cannot fabricate a green.** It moved a verdict in the *opposite*
direction — from `INCONCLUSIVE` to an explicitly named failure. Arm E adds
observation and grades nothing about `cs` at all; it measures Claude Code
by hand, which is what makes the attribution in C vs D falsifiable rather
than assumed.

### M2 — 2026-07-25 · `5587114` · arm C stopped grading a pane

**Changed.** Arm C no longer decides by grepping a captured tmux pane. It
asserts the four observable post-conditions of a correct refusal:

1. `cs tackle` exits non-zero — read from the demoted shell's
   `ARM_C_TACKLE_RC`, not the outer shell's status, which always ends on an
   `echo` and is therefore always 0;
2. stderr quotes the screen it refused, anchored on `cs`'s own
   `Pane showed:` prefix;
3. the tmux session is gone;
4. the molecule is not left `running`.

`run_scenario_2` gained an `expect` argument, because C (virgin) and D
(onboarded) differ by one config key and therefore expect *opposite*
post-conditions. Arm 0's provenance block gained the `awaiting-human`
marker.

**Why.** Once the door-4 work landed, a correct refusal leaves **no pane to
capture**. The pane grep could then only fall into its default branch and
print *"NOT EXECUTABLE — cs tackle created no pane"* over a build that
behaved correctly. An instrument that reports nothing while the build works
is the same surface lie issue #20 is about, wearing the other mask.

**Why it cannot fabricate a green.** It made the arm strictly *harder* to
pass: four conjunctive conditions where there had been one grep, and the
run that introduced it reported **NOT PROVEN** — `cs tackle` exited 0 over
the login-method selector, left the session up and the molecule `running`.
An edit whose own first run is red is not an edit that manufactures green.

### M3 — 2026-07-26 · `73c4b2a` · two observation points the fix removed

Landed in the same commit as the door-4 fix. Two repairs, neither touching
the build under measurement, plus one pure addition.

#### M3a — arm C, assertion 2: which screen the refusal must quote

**Before.** `grep -q "Pane showed:.*Select login method"` — the refusal had
to name the **login-method selector**.

**After.** `grep -q "Pane showed:.*\(Select login method\|Let's get
started\|Choose the text style\)"` — the refusal may name **either
onboarding screen**, still anchored on `cs`'s own `Pane showed:` prefix and
still requiring the real words of a real screen.

**Why.** The belief that the selector was the blocking screen was simply
wrong, and the instrumented run of 2026-07-25 is what showed it. For the
whole 30 s readiness window the pane was Claude Code's **first-run theme
wizard** (`Let's get started.`); the selector appeared only *afterwards*,
when the briefing `cs` typed into the wizard answered it. Every capture the
bench had ever taken was post-return, which is why it only ever saw the
second screen. An assertion demanding the selector would now report a
failure over a build behaving exactly as intended.

**Why it cannot fabricate a green — the structural argument.** The assertion
describes **the content of a refusal**. Before the fix there *was no
refusal*: `cs tackle` exited 0, the session stayed up, the molecule stayed
`running`, and nothing ever printed `Pane showed:`. A test on the wording of
a message that is never emitted cannot make the old behaviour pass. It also
does not stand alone: post-conditions 1, 3 and 4 (non-zero exit, session
torn down, molecule not `running`) are untouched since M2 and each of them
independently fails on the pre-fix build.

**Independent confirmation.** The differential replay runs *this* assertion,
unchanged, against `4c41738`. Arm C is red there. See the report.

#### M3b — arm B: when to read the worktree's owner

**Before.** `stat -c %u` on `$WORK/.worktrees/$MOL`, taken **after** `cs
tackle` returned.

**After.** A read-only background watcher records the **first owner the
worktree ever has**, from the moment `tackle` creates it. The post-hoc
`stat` is still taken and is still **preferred** when it yields a reading;
the watcher's value is used only when the post-hoc read says `(absent)`,
and the report prints both values and says which one it graded on.

**Why.** Since the door-4 fix, a refusal calls `cleanup_partial_tackle`,
which tears down session, branch **and worktree**. The post-hoc `stat` then
reads `(absent)` and arm B can prove nothing — the same pathology as M2,
a third mask: an observation point that the correct behaviour removes.

**Why it cannot fabricate a green.** The watcher runs `stat -c %u` on the
same path as the post-hoc read; it is the identical measurement taken
earlier in the tree's life. It never writes, waits on, or perturbs anything
`cs` does. Arm B's failure condition — the reported provisioning refusal
firing (`UnprovisionedTarget` / `is not usable by it` / `chown the worktree`)
— is checked on `cs`'s output and is untouched by this change: a build that
still refuses is still graded `NOT PROVEN` regardless of what either reader
saw. And the reading itself is not a free parameter: `0` is the reported
bug, `10001` is the proof, and the watcher reports whichever it finds.

#### M3c — the readiness trace, printed (pure addition)

**Changed.** `run_scenario_2` sets `COSMON_READINESS_TRACE` and, after the
arm, prints the trace: one line per sample (elapsed, event, status,
liveness, pane lines) plus the full captured bytes of the first sample of
each distinct status.

**Why.** Arm C's contradiction — the classifier refusing the captured pane
while the live dispatch went through it anyway — is about what the process
saw *during* its window, which no capture taken from outside can settle.

**Why it cannot fabricate a green.** It grades nothing. No verdict reads the
trace; it is reporting only, and the `else` branch prints
`readiness trace: EMPTY or absent` when the binary under test does not
support the variable — which is exactly what happens on `4c41738`, where
`cosmon_transport::readiness_trace` does not exist.

#### M3d — arm D's classifier gained a theme-wizard branch

**Changed.** The onboarded arm's pane classifier now names the first-run
theme wizard (`Choose the text style` / `Let's get started`) as an
`ANOMALY`, beside the pre-existing login-selector `ANOMALY` branch.

**Why it cannot fabricate a green.** It only adds a *failure* label, taken
from the `INCONCLUSIVE` bucket. `PROVEN` for arm D still requires the
composer, unchanged.

---

## A note on the provenance markers

The bench's arm-0 provenance block greps three fix-only strings out of the
shipped `cs`:

    "no usable Claude Code credential"   "hasTrustDialogAccepted"   "awaiting-human"

All three are **already present at `4c41738`** — they came from fixes 1–3,
which landed before the parent of the door-4 fix. They prove the binary is
not the v0.3.0 tag; they do **not** distinguish the two builds of the
differential replay.

The discriminant for door 4 specifically is `COSMON_READINESS_TRACE`, the
env var of `cosmon_transport::readiness_trace` — a module introduced by
`73c4b2a` and absent at `4c41738`.

That fourth grep is deliberately **not** in `in-container-bench.sh`. Adding
it would change the harness, and the differential replay's entire claim is
that the harness did not change between the two passes. The driver
(`scripts/container-worker-doors-differential.sh`) reads the marker from the
image instead, outside the frozen file.
