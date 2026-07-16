# Watchdog — `cs-paste-nudge.sh`

A bash watchdog whose only job is to press **Enter** on cosmon worker panes
that got stuck on an un-submitted Claude Code bracketed-paste prompt. One
state. One action. Deletable in one `git rm`.

> "The scanner presses Enter on prompts the operator would have pressed Enter
> on — and does nothing else." — *delib-20260414-76c6 synthesis, jobs*

This is a **temporary safety net**, not a cosmon feature. It lives in
`scripts/`, not in any crate, and it is expected to be decommissioned the
moment einstein's *intrinsic Propelled substates* land (see
[Decommission](#decommission)).

---

## Philosophy

Read the 9-persona deliberation:
`.cosmon/state/fleets/default/molecules/delib-20260414-76c6/synthesis.md`.

The panel converged on a narrow design envelope:

- **Scope** — one state (UNSUBMITTED-PASTE), one action (Enter×2). Every
  other pane state is detect-only at most (C1).
- **Substrate** — bash over Rust. A Rust crate would outlive its usefulness
  and acquire API stability pressure (C2, `torvalds`, `tolnay`).
- **Cadence** — 20s polling. Operator tolerance for a stuck prompt is
  ~30–60s; 20s × 2-tick dwell = ~40s worst-case response (C3, D5, `carnot`).
- **Gating** — dry-run default, two-tick dwell, same-hash confirmation,
  session-name allowlist, self-exclusion. The irreversibility ratio
  (false-positive Enter vs missed nudge) is ~20–100:1 — conservatism
  dominates (C4, `carnot`).
- **Audit** — every action is an NDJSON line at `.cosmon/watchdog.log`. No
  silent keystrokes (C6).

Anti-requirements (hard — these are in the panel's rejection list):

- Does not authenticate.
- Does not write to `.cosmon/state/`.
- Does not respawn, collapse, or mark molecules.
- Does not act on UNKNOWN / LOGIN / RATELIMIT / APPROVAL / CRASHED.
- Does not extend `cosmon-transport` or add any `cs <verb>`.

## Invocation

### One-shot (ad-hoc, operator triaging a known-stuck fleet)

```bash
# Observe only — log what the watchdog *would* do.
./scripts/cs-paste-nudge.sh

# Apply — actually press Enter.
./scripts/cs-paste-nudge.sh --apply
```

### Narrow to a specific tmux socket and session prefix

```bash
./scripts/cs-paste-nudge.sh --apply \
  --socket fleet-socket \
  --session-regex '^task-20260414-'
```

`--socket` is repeatable; default is `default`. Session regex defaults to
the cosmon fleet prefix set `^(fix-|task-|delib-|mission-|idea-|smoke-|temp-)`.

### Long-running (tmux side-car)

Run in an unrelated tmux socket so the watchdog never sees its own pane:

```bash
tmux -L watchdog new -d -s nudge \
  "cd $(pwd) && ./scripts/cs-paste-nudge.sh --apply --interval 20"
```

### `launchd` (macOS) — boot-time side-car

`~/Library/LaunchAgents/dev.cosmon.paste-nudge.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>                <string>dev.cosmon.paste-nudge</string>
  <key>ProgramArguments</key>
  <array>
    <string>/path/to/cosmon/scripts/cs-paste-nudge.sh</string>
    <string>--apply</string>
    <string>--interval</string><string>20</string>
    <string>--log</string><string>/path/to/cosmon/.cosmon/watchdog.log</string>
  </array>
  <key>RunAtLoad</key>            <true/>
  <key>KeepAlive</key>            <true/>
  <key>StandardOutPath</key>      <string>/tmp/cosmon-watchdog.out</string>
  <key>StandardErrorPath</key>    <string>/tmp/cosmon-watchdog.err</string>
</dict>
</plist>
```

Load: `launchctl load -w ~/Library/LaunchAgents/dev.cosmon.paste-nudge.plist`

### Flags reference

| Flag | Default | Meaning |
|------|---------|---------|
| `--dry-run` | on | Log-only; never press Enter. |
| `--apply` | off | Opt in to pressing Enter on confirmed UNSUBMITTED-PASTE. |
| `--socket NAME` | `default` | tmux `-L` socket allowlist. Repeatable. |
| `--session-regex RE` | cosmon fleet prefix | Session-name allowlist. |
| `--interval SEC` | `20` | Polling cadence. |
| `--log PATH` | `.cosmon/watchdog.log` | NDJSON audit log path. |
| `--once` | off | Scan one tick then exit. Used by the integration test. |

## Detection model

On each tick, the scanner does, per matching pane:

1. `tmux capture-pane -p` → strip trailing blank lines → keep the last 20
   lines. (The visible prompt region; `tail -20` on raw capture lands in
   the empty space below the cursor.)
2. Classify:
   - **UNSUBMITTED-PASTE** iff
     `grep 'Pasted text.*\]\s*\[Pasted text'` **or**
     `grep 'Pasted text.*\+\s*[0-9]+\s+lines'` matches.
   - Otherwise **UNKNOWN** — never actioned.
3. Compute a `cksum` hash of the visible region.
4. Update in-memory ring keyed by `(socket, session, pane)`:
   - Same `(state, hash)` as the previous tick → `dwell += 1`.
   - Otherwise → `dwell = 1`.
5. Act only if `state == UNSUBMITTED-PASTE && dwell >= 2 && --apply`:
   - `tmux send-keys Enter` → `sleep 0.3` → `tmux send-keys Enter`.
   - Append `{action: enter2x}` NDJSON record.
   - Reset dwell to 0 and mark the pane as ACTED so the next tick doesn't
     re-fire on a still-stale capture.

**Self-exclusion** — if `$TMUX_PANE` is set, a pane with the same ID is
skipped by construction. Run the watchdog under a **different tmux socket**
than the fleet to fully isolate it (godel, hawking).

## Audit log

`.cosmon/watchdog.log` is append-only NDJSON. Schema:

```json
{
  "ts":      "2026-04-14T12:34:56Z",
  "socket":  "default",
  "session": "task-20260414-8c12",
  "pane":    "%12",
  "state":   "UNSUBMITTED-PASTE" | "UNKNOWN" | "ACTED",
  "action":  "none" | "would-enter2x" | "enter2x",
  "detail":  "dwell=2 hash=... pid=..."
}
```

Rotation: at 10 MB the current log is rolled to `.cosmon/watchdog.log.1`
and replaced; only one backup is kept.

Tail it:

```bash
tail -F .cosmon/watchdog.log | jq -c '{ts, session, state, action}'
```

## Operator responsibilities

The watchdog is a **power tool with one verb** — operator owns everything
else.

- **Authentication** — `/login` flows are detected elsewhere, if at all.
  The watchdog will never answer an auth prompt. If a worker sits on a
  login screen, you reconnect.
- **Crashes** — the watchdog cannot prove a pane crashed from text alone
  (Σ⁰₁-hard per `turing` / `godel`). If `cs peek` shows a frozen agent,
  that is a human decision: `cs collapse` / `cs stuck` / manual respawn.
- **Approvals and rate limits** — out of scope. Detection is speculation
  without production evidence; action is disallowed.
- **Allowlist discipline** — if you add a new fleet session prefix, update
  `--session-regex` wherever you launch the watchdog. Default is the
  current cosmon prefix set; anything else is invisible to the scanner.
- **Log review** — rotation keeps one backup. If you need a longer history,
  ship the NDJSON stream into your existing log pipeline.

## Decommission

This script is **designed to be deleted.** The panel specified three
measurable triggers; any one retires the watchdog.

| Trigger | Test | Source |
|---------|------|--------|
| **DECOM-1** | 14 consecutive days with zero `enter2x` actions in the log. | `carnot` |
| **DECOM-2** | einstein's intrinsic `Propelled::AwaitingUserSubmit` substate lands in `cosmon-transport` / `cs patrol --propel`. | `einstein` / `hawking` |
| **DECOM-3** | `enter2x` count / `cs evolve` count < 1:1000 sustained for 7 days. | `carnot` |

When any trigger fires:

```bash
git rm scripts/cs-paste-nudge.sh \
       scripts/cs-paste-nudge.test.sh \
       docs/watchdog.md
# …and unload launchd / kill the side-car tmux.
```

A **90-day hard deadline** (per `carnot`) ticks from 2026-04-14. If no
trigger has fired by 2026-07-13, either retire the script anyway or file
an ADR that re-justifies its continued existence (Gödel II — proof of
retention must be external, not scanner-generated).

## References

- Deliberation: `.cosmon/state/fleets/default/molecules/delib-20260414-76c6/`
  (`synthesis.md`, `outcomes.md`).
- Implementation task: `task-20260414-8c12`.
- Architectural invariants: `docs/architectural-invariants.md` — the
  watchdog lives entirely inside the Transactional Core perimeter; it
  reads tmux, writes tmux, appends one flat log. No `cs` verb, no state
  store, no daemon semantics beyond a polling loop.
