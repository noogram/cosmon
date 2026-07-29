<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Does a virgin `CLAUDE_CONFIG_DIR` have to be *matured* before a TUI worker can compose?

**Answer: no — not on this bed. The hypothesis is refuted here, and no maturation
step was added to the spawn path.**

Measured 2026-07-27 on macOS 26.5.2 / arm64, Claude Code **2.1.220**, native
install, against `main` at `c5df44f`. Raw panes and the run log are in
`docs/benches/issue-20-maturation/`. Every measurement below is credential-free;
nothing in this investigation read, wrote, copied or printed a secret.

## The question, and why it was worth an experiment

On COSMON-DEV issue #20 the external tester ran ~20 spawns from a fresh config
directory and never reached a composer. Exactly one dispatch completed hands-off
(125.2 s, artifact written, merged), and it required *both* a config directory
matured by roughly four prior `claude -p` runs *and* hand-seeded consent keys.
The dispatch immediately after it, with nothing touched, was refused at
`status=loading`.

That correlation supports a specific reading: Claude Code, on what it considers
a first run, rewrites `.claude.json` wholesale from its own in-memory state, so
consent seeded into a *virgin* directory is destroyed before the render decision
is taken. If that were true, cosmon's pre-grant would be structurally
insufficient and cosmon would have to mature the directory before pre-granting.

It is a plausible reading. It was also, at that point, an inference and not a
result — the tester said so himself and withdrew two neighbouring hypotheses
rather than have them chased on his say-so. This bench exists to hold it to the
same standard in the other direction.

## The move that makes this cheap: the wizard renders before the login check

"Did the worker reach a composer" sounds like a question that needs a working
login. It does not. Claude Code paints its first-run onboarding wizard *before*
it cares whether a credential exists, so an unauthenticated launch still
separates the two screens that matter:

- a config dir whose consent is **not** honoured paints `Let's get started.` /
  `Choose the text style…`;
- a config dir whose consent **is** honoured paints the composer, with
  `Not logged in · Run /login` demoted to the status line.

Both are reachable with no secret anywhere. That is the whole experiment.

## Step 1 — an unauthenticated run *does* mature a virgin directory

A virgin `CLAUDE_CONFIG_DIR` with no credentials file, one `claude -p`:

```
rc=1
stdout: Not logged in · Please run /login
```

It failed on the credential, as intended — and it had already written its
first-run state on the way there:

| before | after |
|---|---|
| directory empty (0 entries) | `.claude.json`, `projects/`, `sessions/`, `backups/` |

`.claude.json` keys written by the failed run: `machineID`, `userID`,
`firstStartTime`, `migrationVersion`, `opusProMigrationComplete`,
`sonnet1m45MigrationComplete`, `hasResetAutoModeOptInForDefaultOffer`,
`seenNotifications`.

So maturation happens **before** the credential check and needs no credential.
Had this come out the other way, the hypothesis would have been dead on the
spot — a maturation step cosmon cannot perform without a secret is not a fix
cosmon can ship. It came out positive, which is what made step 2 worth running.

Worth naming: `hasCompletedOnboarding` is **not** among those keys. Maturing a
directory does not answer the wizard. The two are independent, which is already
a hint that the maturation reading was never load-bearing.

## Step 2 — a pre-grant written *before* the maturing run survives it

Consent seeded into a virgin directory, then the maturing `claude -p` run:

```
before: {"onboard":true,"projects":1}
after : {"onboard":true,"projects":1,"machineID":true}
settings after: {"skipDangerousModePermissionPrompt":true}
```

Both consent keys survived, and the maturation keys were added alongside them.
The tester's 0.1 s trace of an emptied file (`{"theme":"-","onboard":"-",
"projects":0}` for 113 consecutive samples) does **not** reproduce here. Sampled
at the same 0.1 s resolution across two full launches: 196/200 samples showed
the consent intact, and the 4 that did not were the pre-maturation samples
before `machineID` existed — the consent keys were `true` in all 200.

## Step 3 — the decisive arm, and why no fix was written

Three arms, same workspace, same binary, classified **by cosmon's own
`classify_output`** rather than by eye:

| arm | config dir | screen | `classify_output` |
|---|---|---|---|
| **V0** | virgin, no pre-grant | first-run theme wizard | `Loading` — refused |
| **V1** | virgin, **pre-grant only, never matured** | **composer** | **`Ready`** |
| **M1** | matured by `claude -p`, then pre-grant | composer | `Ready` |

**V1 is the result.** A config directory that has never been matured, carrying
nothing but cosmon's three pre-granted keys, boots straight to a composer and is
graded `Ready` by the same classifier the dispatch gate consults. V0 confirms
the bed really was virgin, so V1 is not an artefact of a dirty directory.

M1 is indistinguishable from V1. Maturation buys nothing here.

Then the acceptance bar itself, on a fresh config dir with cosmon's per-spawn
re-assertion and no human between them: **two consecutive launches, both
composers.** And a zero-gap variant — tear the first session down with no grace
period, re-assert, relaunch immediately — also composed, with 260/260 samples
showing intact consent. The mechanism by which a dying worker's exit-write could
clobber the next dispatch's pre-grant does not fire on this bed either.

So steps 1 and 2 did not support step 3, and **no maturation step was added**.
Shipping one would have meant paying a subprocess spawn per dispatch, with a
timeout and a new typed failure, to fix something not measured to be broken — a
new door of exactly the kind issue #20 has been about. The negative result is
the deliverable.

## What this does *not* settle

The tester's bed and this one disagree, and this bench cannot say why. What it
can do is hand him a probe that localises the disagreement without a secret:
`scripts/claude-pregrant-discriminator.sh` runs the three arms above and prints
one of three verdicts. On his Linux/container bed:

- **V1 composes** → maturation is not his blocker either, and the cause is
  somewhere else entirely (the pane it reaches is then the thing to chase);
- **V1 shows the wizard while M1 composes** → maturation *is* required on Linux
  and not on macOS, the difference is real and platform-scoped, and the fix
  becomes justified with a measurement behind it;
- **neither composes** → both readings are wrong and the script quotes the
  screen actually reached.

The control arm makes a dirty bed self-reporting: if V0 does not show the
wizard, the run declares itself void rather than returning three green arms.

Do not generalise these numbers across operating systems. Every measurement here
is macOS 26.5.2 / arm64 / native install; his is Docker Desktop on macOS running
a Linux container. That is precisely the axis in question.

> **Correction, 2026-07-27.** The second half of that sentence is wrong, and it
> was our error, not his to carry: on 2026-07-27 the tester corrected his own
> earlier description on issue #20 and named his bed as **Colima (Lima-based),
> Ubuntu 24.04.4 LTS, aarch64** — not Docker Desktop. Read the sentence as
> *"his is a Linux container on macOS, under Colima"*. The measurements above
> are untouched and unaffected: they were taken natively on macOS and never on
> either docker engine, so the axis in question is the same axis; only the far
> end of it is now named correctly. The measured difference between the two
> engines is in [`engine-fidelity-2026-07-27.md`](engine-fidelity-2026-07-27.md).

## The half a worker may never close

Full proof needs a **credential in a virgin directory with no interactive
login** — because an interactive login matures the directory as a side effect of
doing anything else, which is exactly what makes our own container run of
2026-07-27 unable to answer this question. Minting or placing a credential is
the operator's gesture and never an agent's.

The command a human runs to close it, on a bed where `cs` is built and
`CLAUDE_CONFIG_DIR` points at a directory that has **never** been logged in:

```sh
# 1. Place a credential in the virgin config dir WITHOUT an interactive login
#    (operator gesture — a worker must never do this step):
export CLAUDE_CONFIG_DIR=/path/to/virgin-config-dir
claude setup-token          # writes the credential; answer the browser prompt

# 2. Confirm the directory is still un-matured — no machineID, no userID:
jq 'has("machineID"), has("userID")' "$CLAUDE_CONFIG_DIR/.claude.json"   # expect: false false

# 3. Two consecutive hands-off dispatches, nothing touched between them:
cs nucleate task-work --var topic="write primes.md, tiny" --json
cs tackle <id-1> --adapter claude
cs tackle <id-2> --adapter claude
```

Two `completed` molecules with no intervention between them is the bar. Anything
less and the question stays open.
