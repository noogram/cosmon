---
name: cmb
description: "Cosmic Microwave Background — emit a copy-pasteable handoff snapshot capturing this session's residual signal (open atomic questions, runners in flight, operator preferences) so a successor session can pick up the work without re-asking."
user_invocable: true
allowed_tools:
  - Bash
metadata: { "openclaw": { "emoji": "🌌", "requires": { "bins": ["jq", "git"] } } }
---

`/cmb` is a **semantic** handoff: it produces a human-readable Markdown artifact
capturing the *meaning* of the current session's unwritten state — atomic
questions still open, runners in flight, operator preferences active only in
this session's mental model. The harness `/handoff` skill is **infrastructural**:
it spins a fresh runtime continuation from the hook. The two compose. Run `/cmb`
to draft the bridge, then `/handoff` (or a manual session restart) to cross it.
The metaphor: a Claude Code session radiates a low-frequency residual signal —
the state-not-on-disk — that is invisible unless deliberately captured. `/cmb`
is that capture.

## When to use

- Long session approaching context compaction; about to spin a successor.
- Mid-session pivot between galaxies (cosmon → mailroom → smithy).
- Cross-galaxy delib chain — child session needs the *register* of the parent
  deliberation, not just its synthesis.
- Operator suspects oral-only context will be lost (decisions tranchées,
  ad-hoc preferences not yet inscribed in `MEMORY.md`).

## What the skill does — workflow

1. Run `~/.claude/skills/cmb/cmb.sh` and print its stdout verbatim.
2. **Do not modify the output**. The header line `⚠️ REVIEW BEFORE PASTE` is
   load-bearing — the operator's eye is the final filter against leaked
   reasoning being pasted into a successor.
3. Tell the operator: copy the block, paste into the successor session as the
   first user message. Trim sections that don't apply.

```bash
~/.claude/skills/cmb/cmb.sh                          # intra-galaxy (V0.5)
~/.claude/skills/cmb/cmb.sh --target=mailroom --intent=review   # cross-galaxy (V1')
~/.claude/skills/cmb/cmb.sh to mailroom --intent=none           # positional alias
```

That's the whole gesture. The script handles galaxy detection, session
discovery, deterministic extractors, and the atomic-question heuristic in pure
shell (no Python, no LLM call).

## Cross-galaxy routing (V1')

When the snapshot is destined for *another* galaxy's session, two flags
flip the script into cross-galaxy mode:

- **`--target=<galaxy>`** — name of the receiving galaxy. Validated
  dynamically against `/srv/cosmon/*/.cosmon/config.toml`; unknown names
  exit non-zero with a close-match suggestion and the full known list.
  `to <galaxy>` is a positional prose alias for the same flag.
- **`--intent=<verb>`** — required when `--target` is set. Closed verb
  set:
  - `pick-up` — start work on the snapshot's referenced artefacts
  - `review` — read and produce feedback
  - `extend` — add to an existing artefact (delib, ADR, doc)
  - `respond` — reply to an open atomic question
  - `inform` — context-only, no action expected
  - `none` — explicit honest exit (synonymous with omitting `intent`;
    valid when emitter genuinely has no verb in mind — *« reste honnête »*).
  - **`merge` is explicitly forbidden.** Merge-before-dispatch
    (CLAUDE.md / ADR-016) makes `cs done` the only path to fusion. A
    cross-galaxy `merge` intent would create a bypass — the receiver
    might fuse work that has not been validated by the source galaxy's
    `cs done` gate. The script exits non-zero with that rationale.

When `--target` is set, the script:

1. **Adds `to: <galaxy>` and `intent: <verb>` to the frontmatter** —
   re-internalizing the routing information that copy-paste otherwise
   removes (delib-20260509-5ce6 §3, D2).
2. **Gates source-galaxy noise sections.** Omitted from cross-galaxy
   output:
   - `### Working tree` (git status of source)
   - `## Runners in flight` (cs ensemble — source-fleet specific)
   - `### Active tmux sessions` (system-wide, off-galaxy noise)
   - Galaxy-local entries in `MEMORY.md` (those whose file slug starts
     with `project_` — pragmatic prefix rule; future operator-prefix
     index markers can refine this when the convention lands).
3. **Universal sections stay** — the `⚠️ REVIEW BEFORE PASTE` lede,
   `## Atomic questions OPEN`, `## Decisions tranchées (candidates)`,
   global `MEMORY.md` preferences, and the autopilot kill-switch
   state.

The frontmatter under `--target` extends the V0.5 4-field cap with two
routing fields (`to:`, `intent:`) — not descriptive sprawl but the
re-internalized path information that copy-paste removes (synthesis
§3 D2). The ADR amendment lives in sibling task `task-20260509-3ba1`,
not here.

## What the skill captures

Mechanically-extracted, deterministic:

- **Galaxy + worktree + branch** (walk-up `.cosmon/config.toml`, then `git`).
- **Recent commits** (`git log --oneline -5 --first-parent`).
- **Working tree** (`git status -sb`).
- **Live workers in fleet** (`cs ensemble --json`, only when in a cosmon
  worktree; degrades to `_(not in cosmon)_` outside).
- **Active tmux sessions** (system-wide).
- **Autopilot kill-switch state** (`~/.cosmon/autopilot.off`).
- **Operator-preference index** from the project-level `MEMORY.md`.

Heuristic, may include false positives (favored — operator prunes on paste):

- **Atomic questions OPEN** — assistant text messages ending with `?` whose
  next user reply is *not* a short affirmative (`oui`, `non`, `ok`, `d'accord`,
  `merci`, …) are tagged `open`. If no user reply at all, tagged `no_reply`
  (the agent likely moved on with tool calls). Capped at 5 most-recent.
- **Decisions tranchées (candidates)** — V0.5. User messages classified by
  three orthogonal signal patterns, first match wins:
  - **`verb`** — strong, low-ambiguity decision phrases (`gardons`,
    `on garde`, `acté`, `on tranche`, `on retient`, `we keep`,
    `we ship(ped)`, `ship it`, `let's ship`, `let's keep`, `merge it`,
    `on (le) merge`).
  - **`mode-feedback`** — enduring-rule patterns (`don't`, `never`,
    `always`, `from now on`, `désormais`, `à partir de maintenant`,
    `ne fais pas`, `par défaut`, `default to`, `fais toujours/jamais`).
  - **`short-reply`** — user message between 3–19 words, immediately after
    an assistant `?`, that is not a pure verdict (`oui`/`non`/`ok`/…).
  Counter-questions (`?` at end), system-wrapped messages
  (`[Request interrupted...]`, slash-commands, image-only), and long pastes
  (>800 chars) are filtered out. Capped at 10 most-recent.

## What V0/V0.5/V1' do NOT do — by design

- **No auto-write to disk.** Output goes to stdout only. The
  agent-to-agent direct-write case (e.g. session-A writing to
  `<Y>/docs/cmb/inbox/`) uses the session-A `Write` tool directly; that
  path is a separate concern and is not part of this skill.
- **No state mutation.** No `cs nucleate`, no `cs evolve`, no `cs done`.
- **No hook, no `--blocking-hook`.** Manual gesture only — typing
  `/cmb` is the meaningful moment (ADR-091 §6 forbids blocking hooks).
- **No outbox directory.** ADR-091 §6 forbids per-galaxy outbox state.
- **No `read_seal`, no acknowledgment-by-writeback.** V2-if-observed.
- **No `merge` verb in `--intent`.** Merge-before-dispatch (CLAUDE.md /
  ADR-016) makes `cs done` the only path to fusion; a cross-galaxy
  `merge` intent would create a bypass.
- **No verdict-payload pairing.** When the operator gates an agent
  recommendation with `oui` (pure verdict, filtered out), V0.5 does not
  surface the agent's preceding proposition. That richer pairing is a
  future V0.6 transformation.
- **No verb-form disambiguation.** "On fait ça?" (question) and
  "on fait ça." (decision) share lexical form; V0.5 drops the verb
  family entirely rather than guess. Same for bare French
  `toujours`/`jamais` (status vs rule), kept only in verb-led forms
  (`fais toujours/jamais`).

## Discovery — finding the current session JSONL

The script probes both layouts:

1. `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` (Claude Code
   convention; the directory name encodes `$CWD` with `/` replaced by `-`).
2. `~/.openclaw/agents/<agentId>/sessions/<session-id>.jsonl` (older openclaw
   convention).

For each candidate, it reads the first non-null `cwd` field from the JSONL and
keeps the one whose `cwd` is the **longest prefix** of `$PWD` (with mtime as
tiebreak). Same gesture as discovering an Obsidian vault by walking up.

## Risks and mitigations

- **Heuristic noise** — false positives possible. Question heuristic capped
  at 5; decision heuristic capped at 10. Operator prunes on paste.
- **Session-discovery brittleness** — if `~/.claude/projects/` layout changes
  in a future Claude Code version, the script falls back to
  `~/.openclaw/agents/`, then degrades cleanly with a warning. The
  deterministic extractors (git, cs) still work.
- **Operator paste-and-forget** — first line of the output is
  `⚠️ REVIEW BEFORE PASTE — auto-extracted, may include leaked reasoning`.
  Same gesture as a release-note draft: never ship without a human eye.
- **JSONL schema drift** — Claude Code stores plain user text as a string
  on `.message.content`, but tool results, image messages, and assistant
  blocks use an array. The `texts_of` normalizer handles both shapes; if
  a future schema introduces a third form, the heuristics degrade to
  empty rather than crash.

## Empirical validation (V0.5)

Pass run on five recent conversational sessions (cosmon, mailroom,
smithy, workshop, tenant-demo), full-session scan with the V0.5 extractor:

| Session | Candidates | TP | FP | Pattern that triggered |
|---|---|---|---|---|
| cosmon main | 0 | 0 | 0 | (decisions gated by pure `oui` — by design unhandled) |
| mailroom | 0 | 0 | 0 | — |
| smithy | 0 | 0 | 0 | — (long Element-paste correctly filtered by 800-char cap) |
| workshop | 1 | 1 | 0 | `verb` on `shipped` (relayed cmb V0 status, real meta-decision) |
| tenant-demo | 0 | 0 | 0 | — |

Empirical precision = 1/1 (small N — single hit). False positives that
earlier appeared in pre-tightening drafts (`toujours rien`, `Verdict
toujours vide`, `on fait ça?` inside parenthetical questions) are
eliminated by the regex tightening and the 800-char filter.

V0.5 is intentionally precision-biased: low recall is acceptable because
the operator can manually fill the section, but each surfaced candidate
should not require deletion. The verb-form ambiguity ("on fait ça?" vs
"on fait ça.") is unsolvable in pure regex; V0.5 drops the family rather
than guess.

## Empirical validation (V1' — `--target` / `--intent`)

Run `/cmb --target=<G>` on at least 3 conversational sessions and
tabulate. Pass = no regression on the V0.5 column ; new `--target`
column shows correct gating (Runners-in-flight / Active-tmux /
Working-tree absent; `project_*` MEMORY entries filtered;
`to:` + `intent:` present in frontmatter).

| Session | `--target` | sections gated correctly? | `intent` validation triggered? |
|---|---|---|---|
| cosmon worker `task-20260509-b621` | `mailroom` (`--intent=review`) | ✅ Runners / tmux / Working-tree absent ; 0 `project_*` MEMORY leaks ; `to:` + `intent:` present | ✅ unknown verb → exit 2 ; `merge` → exit 2 with merge-before-dispatch rationale ; missing intent → exit 2 |
| (operator session #2) | _to fill on next cross-galaxy use_ | | |
| (operator session #3) | _to fill on next cross-galaxy use_ | | |

Rejection-path smoke tests (all exit 2):

- `cmb --target=unknown-galaxy` → close-match suggestion + galaxy list.
- `cmb --target=mailroom` (no intent) → "intent required" + verb list.
- `cmb --target=mailroom --intent=merge` → merge-before-dispatch
  rationale + verb list.

## Genealogy

- Idea: `idea-20260508-819f` (parent capture + feasibility + plan).
- ADR: `/srv/cosmon/cosmon/docs/adr/091-cmb-handoff-pattern.md` (the *pattern*,
  galaxy-agnostic).
- V0: `task-20260509-1ca4` (skill skeleton + atomic-question heuristic).
- V0.5: `task-20260509-dea0` (decisions-tranchées extraction).
- V1': `task-20260509-b621` (`--target` / `--intent` cross-galaxy routing —
  this task). Parent deliberation: `delib-20260509-5ce6`. Sibling
  follow-ups: `task-20260509-3ba1` (ADR amendment),
  `task-20260509-7d27` (operator-decision doctrine),
  `task-20260509-16f9` (doc-pédagogique).
