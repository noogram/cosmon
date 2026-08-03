<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# External rate measurement — issue #39, 2026-08-02

Raw data behind the rate bound reported on
[#39](https://github.com/noogram/cosmon/issues/39). Run by an external tester
on an independent bench, using this directory's harness.

> **Corrected 2026-08-03.** The first version of this README published pooled
> bounds and described the single busy-arm event as a reproduction. Both were
> wrong and are retracted below. See the
> [correction](https://github.com/noogram/cosmon/issues/39#issuecomment-5165111213).

## Result

Exact one-sided 95 % upper bounds, **verifiable data only** — the files in this
directory and nothing else.

| stratum | n | unsubmitted | 95 % upper bound |
|---|---|---|---|
| cold containers, idle — the arm #39 specified | 300 | 0 | **0.994 %** |
| `--busy`, box under deliberate load | 98 † | 1 ‡ | ≤ 4.75 % |

† 100 attempted; 2 errored `composer never rendered` (startup timeouts under
load, excluded as unusable rather than scored as failures).

‡ **Not a confirmed failure — see the next section.**

**The strata are not pooled.** An earlier version quoted a pooled 0.593 %,
which averages the terrain where the defect might live with the terrain where
it does not. That version also folded in a run whose raw data no longer exists,
in the same document that says only the files here count as evidence. Both are
withdrawn.

The arm #39 asked for — cold starts and container panes — is clean at 300
trials, which meets the target the issue set.

## The busy-arm event, and why it is not a reproduction

```json
{"size_lines": 1, "delay_ms": 500, "rep": 18, "busy": true,
 "cr_after_paste": true,
 "paste_to_cr_actual_ms": 556,
 "clear_after_cr_ms": null,
 "pending_after_settle": true}
```

It was first reported as a delivered-but-dropped carriage return. **It does not
support that claim.** The same file contains:

```
size=1  rep=18   clear_after_cr_ms = null      ← scored "unsubmitted"
size=1  rep=21   clear_after_cr_ms = 23,813 ms ← same size, same arm, same load
```

A sibling one-line paste under identical conditions took **23.8 seconds** to
clear, against a 4 s settle window (`--settle-s` default). "Did not clear
inside the window" and "the CR was delivered and dropped" are different
propositions, and only the first is supported.

The window is also not reliably 4 s. `matrix.py` polls
`while time.time() < deadline` and each iteration shells out to
`capture-pane`; under a load average of 8.84 that subprocess can block for many
seconds, which is how a 23,813 ms clear is recorded inside a nominal 4 s budget
at all. On a loaded box `pending_after_settle` is least trustworthy exactly
where the interesting cases are.

**A future run in this terrain wants `--settle-s 30` and the typed `event_ack`
receipt (`8749887`), which distinguishes "application accepted the prompt" from
"composer looked empty".** That distinction is what this issue turns on, and no
composer-text heuristic can supply it.

Also retracted: the `⏸ manual mode on` line in that trial's `pane_tail` was
reported as if it were context. It is a deterministic function of paste size in
Claude Code 2.1.220 — upstream's 232-trial corpus shows it on 82/82
`size_lines == 1` trials including 49 successful ones, and the 300 idle records
here show the same. It carries no information about the outcome.

## Provenance

The files here are **run 2**. An identically configured run 1 (300 idle + 100
busy, both clean) was destroyed by the tester's own sandbox cleanup before
publication; its aggregates appear in the issue thread and are **not**
re-derivable from anything here. Only run 2 is evidence, and the bounds above
use run 2 alone.

## Environment

```
cs        0.5.0 (971f75c5, tree 7abdfaf1)   signed release, cosign-verified before use
claude    2.1.220 (Claude Code)
tmux      3.3a          python 3.11.2
kernel    Linux 6.8.0-100-generic aarch64   10 cpus / 19.9 GB
engine    Colima (Lima), docker 29.2.1, container-local storage
trials    every trial a cold start: new tmux session, new claude process, torn down after
grid      delay 500 ms; sizes 1, 12, 100, 300 lines; 75 reps (idle) / 25 reps (busy)
window    2026-08-02T19:47Z → 20:4xZ
load      sampled every 30 s: min 0.26, median 4.21, max 8.84 (see load.log)
```

The busy arm ran with six CPU spinners pinning the box deliberately, because a
zero on an idle bench is weak evidence about the terrain where these races were
historically seen (#20).

## Harness modification

`matrix.py`'s `wait_ready` could not see a ready composer on Claude Code
2.1.220 and every trial reported `composer never rendered`. The check required
the composer's first line to *equal* a bare glyph, but 2.1.220 renders a
rotating placeholder beside it (`❯ Try "fix lint errors"`). Only that function
was changed; `composer_region` and `composer_pending` were already correct
because they use `startswith`.

`wait_ready.patch` is the exact change, applied for both runs. If the shipped
harness has since been corrected, prefer it and treat this patch as a record of
what was actually executed.

Both controls passed on the patched harness before either arm ran:

```
--no-cr   pending=true   cr_at_pty=false   4/4    detector can see an unsubmitted briefing
with CR   pending=false  cr_at_pty=true    4/4    clear ~304 ms
```

The `--no-cr` control is what makes the zeros meaningful; without it a grid of
`pending=false` would be vacuous, per this experiment's own README.

## Files

| file | contents |
|---|---|
| `idle-300.jsonl` | 300 trials, delay 500 ms, idle box |
| `busy-100.jsonl` | 100 trials, delay 500 ms, `--busy`, box under load |
| `control-no-cr.jsonl` | paste-and-never-submit control |
| `control-with-cr.jsonl` | positive control |
| `load.log` | 1/5/15-minute load averages, sampled every 30 s |
| `wait_ready.patch` | the harness change described above |
