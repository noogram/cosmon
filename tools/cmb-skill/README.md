# `/cmb` skill — Cosmic Microwave Background session-handoff

User-global Claude Code skill that emits a copy-pasteable Markdown handoff
prompt capturing the **residual signal** of the current session — open atomic
questions, runners in flight, operator preferences, recent commits — so a
successor session can pick up the work without re-asking.

## Install

```bash
./install.sh
```

Idempotent. Copies `SKILL.md` + `cmb.sh` into `~/.claude/skills/cmb/`.

## Use

In a Claude Code session, type `/cmb`. Claude runs `~/.claude/skills/cmb/cmb.sh`
and prints the result. Copy the output, paste it as the first user message of
the successor session.

For a cross-galaxy handoff (V1'):

```
/cmb --target=mailroom --intent=review
```

The `--target=<galaxy>` flag (with `to <galaxy>` as positional alias)
gates source-fleet noise sections, filters galaxy-local memory entries,
and emits `to:` + `intent:` in the frontmatter. `--intent` is required
when `--target` is set; verbs are `pick-up | review | extend | respond
| inform | none`. `merge` is forbidden — see SKILL.md for the
merge-before-dispatch rationale.

## Source of truth

This directory is the canonical source. The deployed copy at
`~/.claude/skills/cmb/` is a render-target, never edited in place. To change
the skill: edit here, run `./install.sh`, commit.

## Distinction from harness `/handoff`

- `/handoff` (harness built-in) is **infrastructural** — spins a fresh session
  from the hook, continuing the runtime.
- `/cmb` is **semantic** — produces a human-readable artifact capturing the
  *meaning* of the session's unwritten state.

The two compose: run `/cmb` to draft the bridge, then `/handoff` (or a manual
session restart) to cross it.

## Genealogy

- Idea: `idea-20260508-819f` — capture, feasibility, plan.
- ADR: `docs/adr/091-cmb-handoff-pattern.md` — the *pattern*, galaxy-agnostic.
- Task (V0): `task-20260509-1ca4` — skill skeleton + atomic-question heuristic.
- Task (V0.5): `task-20260509-dea0` — decisions-tranchées extraction.
- Task (V1'): `task-20260509-b621` — `--target` / `--intent` cross-galaxy routing.

## Implementation notes

Pure shell — `bash` + `jq` + `git`. No Python, no Rust, no LLM call.
Discovers its own session JSONL by longest-prefix-match on the `cwd` field
across both `~/.claude/projects/` (Claude Code) and `~/.openclaw/agents/`
(older openclaw layout).

The atomic-question heuristic favors **false positives** (operator prunes
on paste) over false negatives (silent loss). Capped at 5 most-recent
apparent-open questions.

V0.5 adds a decision-candidates heuristic with three orthogonal signal
patterns (`verb`, `mode-feedback`, `short-reply`), filtered for
counter-questions, system-wrapped messages, and long pastes. Capped at 10
most-recent. Precision-biased — see `SKILL.md` for the empirical
validation table and the explicit non-goals (verdict-payload pairing,
verb-form disambiguation).
