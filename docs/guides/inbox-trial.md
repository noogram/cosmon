# Inbox — 7-day operator trial protocol

> `cs inbox` is the first step of cosmon *sortant du nid*. The trial
> measures whether the mailbox is dense enough to replace the Claude
> Code habit — or whether it is a dashboard in disguise. One week of
> discipline, two numbers at the end.

**Source documents:**
- Parent deliberation: `.cosmon/state/fleets/default/molecules/delib-20260422-f6d6/synthesis.md`
- Jobs response: `…/delib-20260422-f6d6/responses/jobs.md` — "Le cockpit,
  c'est une boîte aux lettres"
- Niel response: `…/delib-20260422-f6d6/responses/niel.md` — "On coupe
  le câble"

## What we are testing

One binary question: **is the inbox the new cockpit, or an ornament on
the side of Claude Code?**

Two numbers answer it.

| Signal | Measured by | Goal |
|---|---|---|
| **Binary sevrage (Jobs)** | days/7 the operator did not open Claude Code to pilot cosmon | ≥ 5/7 |
| **Continuous compression (Niel)** | ratio of pilote tokens ÷ worker tokens over the week | ≤ 10:1 |

Jobs tells you whether we cut the cable. Niel tells you whether we
really internalised the clock. Both must land — 5/7 with a 40:1 ratio
means the operator is still burning cycles somewhere else. 15/7 with a
3:1 ratio means a bug in the measurement.

## Counting "days without Claude Code"

The operator has **two reasons** to open Claude Code:

1. **Pilotage** — reading the fleet state, dispatching molecules,
   deciding whether to merge. This is what inbox aims to replace.
2. **Work** — writing code *as* a worker inside a worktree (molecule
   implementation sessions). These do **not** count. Workers are
   allowed to keep using Claude Code; the trial only covers pilotage.

### How to count, operationally

Every morning for seven days, run:

```bash
cd ~/.claude/projects/-Users-you-galaxies-cosmon
ls -la *.jsonl | awk '{print $9, $6, $7, $8}'
```

Claude Code writes one `.jsonl` per session under
`~/.claude/projects/<slug>/`. The file's mtime is the "last-touched" of
that session. A day *without pilotage* is a day whose mtimes on
non-worker sessions are all older than that day's 00:00.

Record each day as `✓` (no pilotage session) or `✗` (at least one).
A worker-session is identifiable by its working directory prefix
(`.worktrees/<mol_id>/` under the galaxy) — exclude those.

Stretch goal: write `cs doctor pilotage-count` to do this automatically.

## Measuring the token ratio

Worker totals come from the fleet snapshot. `cs ensemble --json` emits
`{workers: [...]}`, and each worker carries the token counts `claudion`
scraped from its Claude Code JSONL:

```bash
cs ensemble --json | jq '[.workers[] | .input_tokens + .output_tokens] | add'
```

The **total** — workers plus any stray pilotage session — has no
one-liner today, and that is a property of the fleet, not a gap in the
CLI: a pilotage session is not a cosmon worker, so no `cs` verb can see
it. `cs peek --json` reports what peek observes (per-molecule `status`,
`heartbeat`, `last_activity`) and deliberately carries no token counts;
asking it for the total would return the worker subtotal wearing the
total's name. Until the stretch goal below lands, measure the total by
scraping `~/.claude/projects/**/*.jsonl` directly — the same files
`claudion` parses — and subtract the worker figure above.

The pilotage ratio is `(total − worker) ÷ worker`. Aim for ≤ 10.

Stretch goal: `cs doctor pilotage-ratio`, so the subtraction stops being
a manual step.

The pre-inbox baseline — to be measured during the first two days of
the trial — is expected to be in the 15:1 to 40:1 band per the parent
delib's framing.

## Daily protocol

1. **Morning.** Open a terminal. `cs inbox`. Read the pile, do not
   open Claude Code. Use `cs session start --galaxy cosmon` to open
   the daily carnet — the sticky line will now appear above the pile.
2. **During the day.** Every time the urge strikes to ask Claude Code
   *"what's pending?"* or *"should I merge X?"*, re-open `cs inbox`
   instead. If the pile does not answer the question, write a `cs
   session note` describing the gap — that is the material that
   tomorrow's iteration on inbox will feed on.
3. **Evening.** `cs session end` — the carnet auto-commits. Record the
   binary Jobs signal (`✓` / `✗`) in a single-line note under
   `docs/guides/inbox-trial-log.md`.

## Acceptance of the trial

- **Jobs ≥ 5/7 AND Niel ≤ 10:1:** inbox shipped. Promote `temp:warm` →
  `temp:hot` on the v3 iteration items (modal detail overlay, kind
  filters, …).
- **Jobs < 5/7:** the pile is not dense enough. Read the session notes
  to find the missed signal; nucleate a `task` to surface it.
- **Niel > 10:1 but Jobs ≥ 5/7:** operator is doing ghost pilotage
  elsewhere (Slack, whiteboard, paper). Find the external channel and
  consider nucleating a `signal-bridge` molecule.
- **Both fail:** write a chronicle explaining why and roll back the
  `Inbox` recommendation to `temp:cold`. The delib gate was
  falsifiable — honor it.

## What not to count as a win

- *"I only opened Claude Code for two minutes"* — any session-start
  counts as a `✗` for the day. The habit is the problem, not the
  duration.
- *"I used `cs ensemble` instead"* — that counts as a `✓`. `cs
  ensemble`, `cs peek`, `cs inbox`, and any combination thereof are
  all inbox-compatible.
- *"I used grep on `.cosmon/state/`"* — fine. The `past` is meant to
  be consulted that way (Jobs' piège: the search bar).

## Appendix — why two metrics, not one

Jobs and Niel were not given a single metric by accident. Jobs measures
the **cognitive cut** (did the operator stop *reaching for* Claude
Code?); Niel measures the **resource cut** (did the operator stop
*burning* tokens in repeated pilotage reprompts?). Both can pass
independently — operator cuts the habit but still burns tokens when
forced to re-consult Claude Code on hard cases, or keeps the habit
but burns fewer tokens per consultation. We want both to pass.
