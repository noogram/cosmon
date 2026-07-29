# The issue-#20 container bench — replaying an outside tester's failures

For two days every public message about issue #20 carried the same honest
reservation: *fixed, but we could not prove it on the reporter's bench.* The
tester then published a complete, secret-free reproduction recipe. This is the
bench built from that recipe, and the method for re-running it.

Read [`claude-worker-in-a-container.md`](claude-worker-in-a-container.md) first:
it explains the doors. This document is only about *proving* they are shut.

## Run it

```sh
# the bench's own colima profile — see "which engine" below before changing it
colima start --profile cosmon-bench --cpu 4 --memory 8 --disk 60 \
  --vm-type vz --mount-type virtiofs --runtime docker
scripts/container-worker-doors-bench.sh | tee /tmp/bench.log
grep '^VERDICT' /tmp/bench.log
```

If that engine is down the driver refuses, prints the line above, and exits
**2 = INCONCLUSIVE**. It does not fall back to another docker context: a bench
that quietly ran somewhere else is invisible in its own log, and that is how the
engine drift corrected on 2026-07-27 got in.

Roughly four minutes of `cargo build --release` on a cold cache, then under two
minutes of arms. `COSMON_KEEP_IMAGE=1` skips the teardown `rmi` for fast reruns.
The driver only ever removes the image tag it created; no pre-existing container
or image of yours is a candidate for deletion.

Artefacts:

| what | where |
|---|---|
| host driver | `scripts/container-worker-doors-bench.sh` |
| image | `docker/container-worker-doors/Dockerfile` |
| the arms | `docker/container-worker-doors/in-container-bench.sh` |
| the readiness trace | `COSMON_READINESS_TRACE=<path>`, `cosmon_transport::readiness_trace` |
| the instrument's own history | `docker/container-worker-doors/HARNESS-CHANGELOG.md` |
| the differential replay | `scripts/container-worker-doors-differential.sh` |
| its report | [`../benches/issue-20-door-4-differential.md`](../benches/issue-20-door-4-differential.md) |

**If you change the harness, add a row to `HARNESS-CHANGELOG.md` in the same
commit.** The bench measures `cs`, so when `cs` changes the instrument
sometimes has to move — a refusal that tears the session down deletes the pane
the bench used to read. That repair is unavoidable, and from the outside it is
indistinguishable from loosening a test until it passes. The register is where
each repair states which observation point moved and why it cannot manufacture
a green. The differential replay is the check that does not require believing
the register: it runs the harness in its *final* state against the fix and
against its parent, and the parent must still be red.

The trace is the instrument that settled door 4. Set the variable on any `cs`
invocation — inside the bench or on your own machine — and the readiness loop
appends one JSON line per sample: timestamp, event, classified status, collapsed
liveness, and the exact pane bytes it classified. Unset, nothing is written and
no file is opened. The bench sets it for arms C and D and prints it before their
verdicts, because *what the process saw* is the observation no capture taken from
outside the process can supply.

## No secrets, by construction

The tester's recipe carries an optional step that seeds a credentials file from
an environment variable. **It is deliberately not transcribed.** Nothing here
reads, mounts, requests, or mints a real token, and no arm can be made to
"work better" by supplying one.

That is not a limitation, because the discriminating observation is free:

> A pane showing the **composer** has passed doors 1 and 2. A pane showing a
> **dialog** has not.

With no valid credential the composer reads `Not logged in · Run /login` — and
that footer *is* the pass signal for the two consent doors. Door 3 is proved by
its refusal, which by definition needs no credential either.

Arms B, C and D do write a **placeholder** `.credentials.json` inside the
container whose token fields are the literal string
`PLACEHOLDER-NOT-A-CREDENTIAL-…` with `expiresAt: 0`. This is not a redaction
of a secret; there is no secret. It exists because door 3's check is
`stat(2)`-only — presence, regular-file-ness, non-emptiness, and the target
uid's read bits — so a placeholder is exactly enough to let the dispatch past
door 3 and make the doors *behind* it observable. Do not "improve" it into a
real token; that deletes the proof rather than strengthening it.

## Engine fidelity — the part that must be checked, not assumed

This section previously reported these two keys as the tester's *readings*, both
`0`:

```sh
sysctl kernel.unprivileged_userns_clone user.max_user_namespaces
```

**Both halves of that sentence were wrong, and the second one was ours.** The
correction (task-20260726-eabf) is worth keeping in full, because the loop it
closes is the worst case of the class this bench exists to catch:

1. Cosmon's own egress warning named those two keys as the cause of a failed
   namespace creation. It had measured neither.
2. The tester never ran that `sysctl`. He read the two key names **out of our
   message**, concluded "both at `0`", and wrote it into his public repro recipe
   as a measurement.
3. We disputed the number by supposing the key is absent from a stock LinuxKit
   kernel and that a swallowed error was being read as a zero.
4. He then measured, in the very container that had produced the report. We were
   both wrong.

What he measured:

```text
kernel.unprivileged_userns_clone = 1        (the key EXISTS on that LinuxKit kernel)
user.max_user_namespaces         = 79654
unshare -Ur true      (root)      -> unshare failed: Operation not permitted
unshare -Ur true      (uid 10001) -> unshare failed: Operation not permitted
unshare --mount true  (root)      -> Operation not permitted
grep Seccomp /proc/self/status    -> Seccomp: 2   Seccomp_filters: 1
docker inspect … SecurityOpt      -> null, privileged=false   (default profile)
```

Both sysctls are healthy. The namespace is refused one layer lower: the engine's
**default seccomp profile** rejects the `unshare` syscall. Not a sysctl, not a
user-namespace policy.

Three things follow, and all of them matter more than the numbers:

- **A functional probe beats a setting read — and it is the only thing that may
  produce a positive claim.** The bench runs
  `setpriv --reuid 10001 … unshare -Ur true`, which is what the sysctls were
  being treated as a proxy *for*. Arm 0 reports it alongside the raw readings,
  and now alongside `/proc/self/status`'s seccomp fields, which is what
  discriminates "sysctl says no" from "sandbox policy says no".
- **A sysctl reading is engine-dependent and not a capability.** `sysctl` on our
  Docker Desktop run reports `kernel.unprivileged_userns_clone` as an unknown
  key while the tester's reports `1`; an unknown key is not a zero, and a `1` is
  not permission. The engine choice changes the reading *and* the reading does
  not decide the outcome — which is why the driver pins one engine and says so
  when you override it.

  > **Which engine, corrected 2026-07-27.** This paragraph used to end "*which
  > is why the driver defaults to `desktop-linux`*", on the belief that Docker
  > Desktop was the tester's engine. It was not: he corrected his own earlier
  > description that day on issue #20 and named Colima (Lima-based), Ubuntu
  > 24.04.4 LTS, aarch64. The driver now pins the dedicated
  > `colima-cosmon-bench` profile, and the difference is not cosmetic — on
  > `desktop-linux`, `unshare` as a non-root uid **succeeds** and a bind mount
  > **honours** `chown`, so neither of the tester's two standing findings can
  > reproduce there at all. Both engines were measured rather than assumed:
  > [`../benches/engine-fidelity-2026-07-27.md`](../benches/engine-fidelity-2026-07-27.md).
  > Note also that the reading quoted just above (`1` on his bed) is now
  > explained: on colima both userns sysctls are permissive and `unshare` is
  > refused anyway, by the default seccomp profile — attributed by flipping
  > `--security-opt seccomp=unconfined` and nothing else.
- **Never name a cause you did not measure.** Cosmon's diagnostic now carries a
  typed blocker (`cosmon_core::egress::NetnsBlocker`) with an explicit
  `Undetermined` variant, so an unattributable failure says *"this probe cannot
  say why"* instead of reaching for a plausible key. Our message is what seeded
  a false measurement into someone else's public bench; that is the cost of
  guessing out loud.

If your run's arm-0 block diverges from the tester's, the bench is not faithful
to his and every verdict below it must be qualified in exactly that way. Say so;
do not average it away.

## What the first run found

Both reported scenarios replay green on the corrected branch, and the bench
turned up a **fourth door**. It stayed open through one more fix — the section
[below](#what-the-instrumented-run-found) is how it was finally shut, and why the
first attempt could not have worked.

| scenario | verdict | evidence |
|---|---|---|
| 1 — worktree ownership catch-22 | **proven** | `cs tackle` exits 0; worktree `stat -c %u` = `10001`; the whole `.cosmon` (state, `fleet.lock`, `events.jsonl`, nucleons, formulas) handed to `cosmon-worker`; the reported refusal never appears |
| 2 — folder-trust door | **proven** | pane reaches the composer, footer `Not logged in · Run /login`; `hasTrustDialogAccepted: true` written for the exact worktree; `skipDangerousModePermissionPrompt: true` written |
| 3 — no credential | **proven** | fail-closed refusal naming the path, the keychain item and `CLAUDE_CODE_OAUTH_TOKEN`; nothing spawns |

Two ownership notes from arm B, both benign but worth not re-discovering:
`fleet.json` and `fleet.runtime.json` stay root-owned after the hand-over. That
is correct — they are written only by dispatcher commands (`save_fleet` is
reached from tackle / done / purge / teardown / ensemble / migrate / init, never
from `evolve` or `complete`), so the demoted worker only ever reads them. And on
a root dispatch the credential is looked for under **root's** `$HOME/.claude`
while its readability is checked against uid 10001; that asymmetry is deliberate
and consistent, because `demotion_command_prefix` omits `--reset-env`, so the
demoted worker keeps root's `HOME` and resolves the same path.

### The fourth door: the login-method selector

Arm C — identical to arm D except for one key — leaves the pane here (verbatim
capture, re-run 2026-07-25 against the door-4 build):

```text
Welcome to Claude Code v2.1.220

 Claude Code can be used with your Claude subscription or billed based on API
 usage through your Console account.

 Select login method:

 ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise
   2. Anthropic Console account · API usage billing
   3. 3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI
```

Three things follow, and the third is the one that matters:

- **This is an onboarding screen, not a credential dialog.** C and D differ only
  in `hasCompletedOnboarding`, so the selector is attributable to onboarding
  alone. Arm E confirms it with cosmon absent: on an *onboarded* dir, both no
  credential (E1) and the placeholder (E2) reach the composer with
  `Not logged in · Run /login`, so the placeholder is not the cause. The
  statement in `claude-worker-in-a-container.md` that *"there is no login
  selector"* holds for the case it measured — an onboarded config dir — and this
  is the case it did not.
- **On 2.1.220 the first onboarding screen is this selector, not the theme
  wizard.** cosmon's markers cover the wizard (`FIRST_RUN_THEME`,
  `FIRST_RUN_WELCOME`) and nothing here matches either.
- **cosmon dispatched over it — and the reason was not on this screen at all.**
  When the door was first named, feeding the captured pane to
  `readiness::classify_output` returned `Ready`, because no marker matched and
  control fell through to a generic last-five-lines `❯` scan that the
  selector's menu chevron satisfied. The readiness fixes (`afb5541`, `97e7eeb`,
  `8094ef4`) closed that: fed the pane above, `classify_output` returns
  `AwaitingHuman`, which `ClaudeTuiProbe::await_live` maps to
  `Liveness::Indeterminate`, which `cs tackle` is written to refuse. Live, it
  did not — arm C kept exiting 0 with the session up and the molecule
  `running`, against a binary whose provenance line reads
  `PRESENT "awaiting-human"`.

  That contradiction was settled by instrumenting the loop rather than by
  reading it again; see [the next section](#what-the-instrumented-run-found).
  The short version: **the process never classified this screen.** For the
  whole 30-second window it was looking at a different one.

A narrower observation from arm D's config: cosmon pre-grants trust for the
worktree, and the galaxy root it passes via `--add-dir` is recorded by Claude
Code as `hasTrustDialogAccepted: false`. It did not raise a dialog in this run,
but nothing pre-grants it either.

None of this weakens the three fixes; it is the next door down the corridor, and
naming it is what the bench is for.

#### Postscript — what shipped at the unit layer, and what it has not proven

The obvious fix was to add `Select login method` to `readiness::markers`. It was
refused. Three doors had already been shut that way, and each time the corridor
stayed open one screen further along — this selector *was* the next unnamed
screen. Naming a fourth would have bought the fourth quiet hang, not the last
one.

The generic last-five-lines `❯` scan — the rule that turned every unnamed menu
into `Ready` — was removed instead. `Ready` is now earned only by positive
evidence that the composer is on screen: the `Type your message` placeholder
sitting on the chevron line, or a non-menu chevron line sharing its tail with the
composer's own footer. A menu-shaped chevron (`❯ 1. …`, `❯ b) …`, `❯ • …`)
produces neither, so arm C's captured pane classifies `AwaitingHuman` — and so
does an invented screen that exists on no build, which is the property that was
meant to close the corridor rather than one door in it.

Four tests pin that layer: `login_method_selector_is_not_ready` on arm C's
captured pane verbatim; `an_unrecognised_menu_is_not_ready` on an invented
screen; `await_live_refuses_a_worker_parked_on_a_menu` on the composed dispatch
verdict, so a refactor that renamed the enum while still dispatching fails there;
and `observe_still_counts_an_unnamed_rendered_screen_as_proof_of_life` on the
other half — the closed default must not leak into the spawn postcondition and
tear down a slow cold start.

**All four were green, and arm C was still red.** That is exactly the state this
postscript recorded, and it was right not to soften it: the classifier refused
the pane the bench captured, and the live dispatch went through it anyway. What
it could not say was *why* — and the reason is in the next section.

The narrower observation still stands unclosed: the galaxy root passed via
`--add-dir` is recorded `hasTrustDialogAccepted: false` and nothing pre-grants
it. It raised no dialog in any arm; it is named here so the next run does not
rediscover it.

## What the instrumented run found

Every explanation on offer for arm C was a reading of the same code, and none of
them could be checked, because nothing recorded what the process *saw*. So the
first change was not a fix. `COSMON_READINESS_TRACE=<path>` (module
`cosmon_transport::readiness_trace`) makes the readiness loop append one JSON
line per sample carrying the classified status **and the exact bytes it
classified**; the bench sets it for arms C and D and prints two projections of
it before any verdict. It is off unless the variable is set, it never fails a
dispatch, and no decision anywhere reads it.

The run of 2026-07-25 answered in two lines:

```text
elapsed  event                       status         note
   …     capture                     loading        (×~60, one per 500 ms)
 30155   wait_ready.return           loading        TIMEOUT — window exhausted
    —    dispatch_gate               loading  live
```

And the bytes behind every one of those `loading` samples were **not** the
login-method selector:

```text
Welcome to Claude Code v2.1.220

 Let's get started.

 Choose the text style that looks best with your terminal
 To change this later, run /theme

   1. Auto (match terminal)
 ❯ 2. Dark mode ✔
   …
```

The chain, end to end:

1. On a virgin config dir the screen on the pane for the whole readiness window
   is the **first-run theme wizard**, not the selector.
2. `classify_output` calls it `Loading` — and that is *correct*: `FIRST_RUN_THEME`
   is a named marker precisely so a cold start caught mid-wizard reads as a cold
   start in progress rather than as nothing recognisable.
3. `wait_ready` deliberately does not answer onboarding, so the window closes
   with the wizard still up, and it returns its last observation: `Loading`.
4. The dispatch gate was a **deny-list** — four statuses forced to
   `Indeterminate`, everything else collapsed through `SessionStatus::liveness`.
   `Loading` was not on the list, so it became `Live`.
5. `cs tackle` dispatched, and `send_input(prompt)` typed an 80-line briefing
   into the wizard. **Those keystrokes answered it**, which advanced the pane to
   the login-method selector.

That last step is why every capture the bench ever took showed the selector: all
of them were taken *after* `cs tackle` returned. The bench was looking at a
screen cosmon's own briefing had summoned. Unit-green and bench-red was not a
paradox — the unit tests and the bench were talking about two different panes,
and the one that decided the dispatch had never been fed to a test.

### The fix, and why it is not a fifth name

The gate is now an **allow-list** (`readiness::dispatch_gate_liveness`): only
`Ready` and `Working` collapse to `Live`; `Dead` stays `Dead`; everything else is
`Indeterminate`. The rule behind it is one sentence — `wait_ready` returns
`Ready` / `Working` *the moment it sees them* and everything else only by
running out of window, so "arrived as evidence" and "is `Ready` or `Working`" are
the same set, and a gate asking *"is this worker accepting work?"* has no
business saying yes to anything else.

Naming the wizard would have been the fifth door named and the fifth corridor
left open. This closes the class: `Loading`, `Unknown`, both consent modals,
`Blocked` and `AwaitingHuman` are refused by one rule, and the match is
exhaustive with no wildcard arm, so a future `SessionStatus` cannot inherit a
side by accident — it breaks the build until someone decides in writing.

Contract C0 is untouched, and that is the thing to check when reading the diff:
`SessionStatus::liveness` still calls a painted frame `Live`, so the *spawn
postcondition* still answers "did the binary run?" with a yes and a slow cold
start is not torn down. Two questions, two answers, as before — one of them is
now honest.

Two tests pin it: `await_live_refuses_a_worker_still_on_the_first_run_wizard_when_the_window_closes`
runs the wizard pane verbatim from this trace through the whole probe and asserts
both halves (postcondition `Live`, gate `Indeterminate`), and
`only_ready_and_working_open_the_dispatch_gate` walks every `SessionStatus` and
pins the collapse by name.

### What re-running the bench changed, beyond arm C

**Arm C is green**: `cs tackle` exits 1, the refusal quotes the screen it
refused, the tmux session is gone, and the molecule is back to `pending`.

**Arm B needed a new observation point, and that is the fix working.** Arm B's
proof is `stat -c %u` on the worktree. It, too, sits on a virgin-ish pane, so it
too now refuses at the readiness gate — and a refusal runs
`cleanup_partial_tackle`, which removes session, branch **and worktree**. A
post-hoc `stat` therefore reads `(absent)` and the arm can prove nothing. That is
arm C's problem wearing a third mask: an instrument whose observation point the
*correct* behaviour removes. Arm B now runs a read-only watcher that records the
first owner the worktree ever has, so the ownership is measured while it exists;
the post-hoc reading is still preferred when there is one, and the report says
which of the two it graded on.

This is the same shape as arm A's note below — the fixes are not independent, and
each new fail-closed gate moves the point at which an older scenario stops being
reachable. Say which gate answered; do not quietly re-point the arm at something
easier to prove.

**A smaller repair the trace forced.** The refusal quotes the pane so the
operator gets a diagnosis instead of a re-run, and it used to quote the last six
non-empty lines. The theme wizard ends in a syntax-highlighting *code sample*, so
that refusal read `console.log("Hello, World!")` and never `Let's get started.` —
a puzzle, not a door name. The quote now carries the pane's head and its tail
with the middle elided, because these screens put their identity in the headline
and their mechanics at the foot.

## The arms, and what each one isolates

> **Arms A and B no longer reach what they were built to isolate.** As of
> 2026-07-28 a root dispatcher with `COSMON_WORKER_UID` set is refused before
> anything is created — see
> [ADR-166](../adr/166-the-root-to-uid-demote-path-is-refused.md). Both arms
> now stop at that refusal, which is earlier than the gates they were measuring.
> This is the same "each new fail-closed gate moves the point at which an older
> scenario stops being reachable" note as above, one gate further along: say
> which gate answered, do not re-point the arm at something easier to prove.

| arm | shape | what it isolates |
|---|---|---|
| A | root, `COSMON_WORKER_UID=10001`, **no credential at all** | door 3's fail-closed refusal — and the gate *ordering* |
| B | root, `COSMON_WORKER_UID=10001`, placeholder credential | scenario 1: the worktree-ownership catch-22 |
| C | `setpriv` to 10001, **virgin** config dir | whether cosmon pre-grants the onboarding gate *itself* |
| D | `setpriv` to 10001, **onboarded** config dir | scenario 2 as the tester actually ran it |
| E | `claude` driven **directly**, cosmon absent | what the placeholder credential alone causes |
| F | **two consecutive dispatches**, one virgin config dir | whether the pre-grant is re-asserted or merely written once |

Arm E exists because arms C and D observe a pane through `cs tackle`, which moves
two variables at once — cosmon's pre-grant and the placeholder credential. E
drives `claude` by hand with both consent gates granted manually, once with and
once without the placeholder, so door 3's real shape on this build is measured
rather than inherited from an earlier session's notes.

Arm A exists because the three fixes are not independent. The credential gate
(`c454422`) sits **first** among the fail-closed gates in
`spawn_claude_and_prompt`, ahead of the consent pre-grant and ahead of
`preflight_root_then_model` — which is where the worktree chown of `9123ec9`
happens. On a container with no credential, the newest fix therefore answers
first, and the older two are never reached. That is correct behaviour and a real
consequence: **the tester's two scenarios are no longer reproducible on a
credential-less container at all**, because cosmon now declines before it gets
there. Arm A measures that ordering instead of leaving it as an inference.

C and D differ by one seeded key, `hasCompletedOnboarding`. A genuinely virgin
`CLAUDE_CONFIG_DIR` renders Claude Code's first-run theme wizard. The tester saw
the trust dialog, so his config was necessarily past onboarding; replaying his
scenario on a virgin dir would have measured a door he never hit and blamed the
wrong fix. Running both is what lets the report tell them apart.

### The 2.1.220 report, and why arm C's expectation flipped

The paragraph above described a bench where cosmon deliberately did **not**
pre-grant onboarding. That changed with the 2.1.220 report: the installer moved
under us, the tester's worker landed on the theme wizard, and cosmon refused —
correctly, loudly, and with the work still not done. `claude_trust` now
pre-grants `hasCompletedOnboarding` alongside folder trust, before every spawn.

So arm C, the virgin-config arm, no longer expects a refusal. It expects a
**composer**, reached because cosmon wrote the key itself. C and D now converge
on the same outcome by two routes — D inherits the key, C has cosmon write it —
and that convergence is the proof. Expecting a refusal there would report red
over the fix working.

This does not loosen §8v. The classifier is untouched, no marker was added for
the wizard, and any screen cosmon cannot certify is still refused. What changed
is upstream: the wizard is not *answered*, it is not *rendered*. Writing consent
into a config file before the process exists is the folder-trust pre-grant's
gesture, not the piloting of onboarding the invariant forbids.

### Arm F — two consecutive dispatches, and why one is not enough

Arm F is the acceptance criterion of the 2.1.220 report, and no other arm can
stand in for it.

Claude Code does not edit `.claude.json`; it rewrites the whole file from its own
in-memory state when a session ends, dropping keys the running build does not
recognise. Measured on 2.1.220 the onboarding and trust keys survived that cycle
while `theme` and `bypassPermissionsModeAccepted` were stripped — and the tester
measured `hasCompletedOnboarding` itself going to `null` on his bench. Same
mechanism, opposite outcome for the key that matters, which is the point: the
survivor set is a moving target.

A pre-grant is therefore an assertion re-made before every spawn, not an install
step — and a run-once design passes dispatch 1 and fails dispatch 2. Every other
arm here dispatches once and cannot see the difference. Arm F dispatches twice
into one pristine config dir with **nothing in between**, tearing the first
worker down first so the config rewrite actually happens, and grades both panes.
Green C/D with a red F would mean precisely "the grant worked once and
evaporated".

The same property is pinned outside the container, against the installed binary,
by `cargo test -p cosmon-transport --test claude_consent_live -- --ignored`.

### Why arm C used to grade a refusal and not a pane

*(Kept for readers of earlier bench runs; arm C now grades a composer, above.)*

Arm C used to decide its verdict by grepping the captured tmux pane. Once the
door-4 fix landed that stopped being an instrument, because the *correct*
behaviour is that there is no pane: `cs tackle` is supposed to decline and tear
the session down. A pane-shaped test over a build that behaved would find
nothing to grep, fall into its default branch, and print *"NOT EXECUTABLE — cs
tackle created no pane"* — a build working perfectly and an instrument saying
"no idea". That is the same surface lie this issue is about, wearing the other
mask, which is why closing it came before anything else.

Arm C now asserts the four post-conditions of a correct refusal, each reported
on its own line before the verdict:

1. `cs tackle` exits **non-zero** — read from the demoted shell's
   `ARM_C_TACKLE_RC`, never from the shell's own status, which always ends on
   an `echo` and is therefore `0` even when tackle refused;
2. its stderr **quotes the login-method selector**, anchored on the refusal's
   own `Pane showed:` prefix so the arm cannot credit cosmon for a line the
   bench itself printed;
3. the **tmux session is gone**, probed on the socket and session name cs
   itself printed — via `attach:` when it believes it succeeded, via the
   `Inspect with tmux -L … -t …` hint when it declines;
4. the **molecule is not left `running`**.

When a session survives, the arm captures it twice — once inside the demoted
shell the instant `cs tackle` returns, once 25 s later — and prints both. Two
captures rather than one because they answer different questions: the same
screen twice means the classifier accepted that screen, while composer-then-menu
means the classifier was right about the frame it saw and the screen changed
under a verdict sampled once. The arm-0 provenance block carries
`awaiting-human` for the same reason: without it, a red arm C cannot tell
"the fix is missing from this binary" from "the fix is present and
insufficient".

## Reading the output

Each arm prints its raw observations — exit status, full stderr, `stat -c %u` of
the worktree, the captured pane — and then one `VERDICT` line. The script never
asserts-and-dies: a surprising observation has to reach the report intact, so
every arm always runs and the script exits 0 unless the harness itself broke.
`INCONCLUSIVE` and `NOT EXECUTABLE` are first-class verdicts. A missing proof
named honestly is worth more than a supposed one — that reservation is the whole
reason this bench exists.
