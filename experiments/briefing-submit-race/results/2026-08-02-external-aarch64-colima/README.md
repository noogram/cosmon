<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# External rate measurement — issue #39, 2026-08-02

Raw data behind the rate bound reported on
[#39](https://github.com/noogram/cosmon/issues/39). Run by an external tester
on an independent bench, using this directory's harness.

## Result

| stratum | usable | unsubmitted | 95 % upper bound |
|---|---|---|---|
| cold containers, idle — the arm #39 specified | 600 † | 0 | **0.498 %** |
| `--busy`, box under deliberate load | 198 † | **1** | 2.37 % |
| pooled | 798 † | 1 | 0.593 % |

† Totals span two runs. The files here are **run 2** (300 idle + 100 busy);
run 1 was an identical configuration whose raw output was destroyed by the
tester's own sandbox cleanup before it was published. Run 1's aggregates
(300 idle / 0, 100 busy / 0) are in the issue thread and are **not**
re-derivable from anything in this directory. Only run 2 is evidence here.

The arm #39 asked for — cold starts and container panes — is clean. The single
failure is in a `--busy`-under-load arm the tester added.

## The failure

```json
{"size_lines": 1, "delay_ms": 500, "rep": 18, "busy": true,
 "cr_after_paste": true,
 "paste_to_cr_actual_ms": 556,
 "clear_after_cr_ms": null,
 "pending_after_settle": true}
```

`cr_after_paste: true` — the carriage return reached the application PTY and is
on disk in the `ptyspy` log — and the composer still held the text after the
settle window. This is the race this experiment was built to distinguish, seen
at the **shipped 500 ms** setting rather than only at 0 ms, on a **one-line**
paste.

`busy-100.jsonl`, `rep` 18, `size_lines` 1.

## Not counted as failures

- **2 trials** errored `composer never rendered` — startup timeouts under load;
  the trial never reached the point of testing delivery. Excluded as unusable
  (100 attempted → 98 usable) rather than scored.
- **1 trial** recorded `cr_after_paste: false` while the composer cleared
  anyway. More likely an instrument gap under load than a real event, so it is
  flagged rather than counted either way. Both records are in the data.

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
historically seen (#20). Run 1's busy arm peaked at 6.31 and found nothing; run
2's peaked at 8.84 and found the event above.

## Harness modification

`matrix.py`'s `wait_ready` could not see a ready composer on Claude Code
2.1.220 and every trial reported `composer never rendered`. The check required
the composer's first line to *equal* a bare glyph, but 2.1.220 renders a
rotating placeholder beside it (`❯ Try "fix lint errors"`). Only that function
was changed; `composer_region` and `composer_pending` were already correct
because they use `startswith`.

`wait_ready.patch` in this directory is the exact change, applied for both
runs. The upstream fix is tracked separately — if the shipped harness has since
been corrected, prefer it and treat this patch as a record of what was actually
executed.

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
