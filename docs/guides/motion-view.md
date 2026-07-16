# Motion — "molécules en mouvement"

The Motion view is the live cockpit image of the local galaxy cluster.
One surface, three skins: an HTTP endpoint (`GET /motion` on cs-api), a
terminal command (`cs motion`), and a tab in the Mac and iOS pilots.

Every skin reads the same filesystem scan — there is no daemon, no
database, no polling loop on the server side. The operator's surface
polls every 3 seconds and the response is a flat JSON document capped
per galaxy so it cannot blow up.

## Five sections

The response envelope has five independent arrays. Each section
answers a different operator question:

| Section | Answers | Source |
|---------|---------|--------|
| `workers` | Which tmux workers are live right now? | `<galaxy>/.cosmon/state/fleet.json` (+ `fleet.runtime.json` for the worktree path) |
| `running_molecules` | Which molecules are advancing step-by-step? | `<galaxy>/.cosmon/state/fleets/*/molecules/*/state.json` filtered on `status=running` |
| `recent_git_commits` | What just landed in every galaxy? | `git log --since=<window>` at each galaxy root |
| `recent_whispers` | What did the operator just say? | `<galaxy>/.cosmon/whispers/inbox/**/*.md`, window ≥ 30 min |
| `recent_sparks` | What ideas were just captured? | molecules whose id starts with `spark-` and whose `created_at` is within the window |

## HTTP API

```sh
curl -s 'http://localhost:4222/motion?window=15m' | jq
```

Query parameters:

- `window=15m` — time span for "recent" sections. Accepted units:
  `s`, `m`, `h`, `d`. Missing or unparseable input falls back to 15 m.
- `galaxies=cosmon,mailroom` — optional allowlist. When omitted
  every directory under `--galaxies-root` with a `.cosmon/` subtree is
  scanned.
- `include=workers,molecules,commits,whispers,sparks` — optional
  section selector. Omitted sections are still rendered as empty
  arrays so clients never have to pattern-match on presence.

### Caps

Each array is capped per galaxy so the response stays bounded:

- `running_molecules`: 50 per galaxy
- `recent_git_commits`: 20 per galaxy
- `recent_whispers`: 50 per galaxy
- `recent_sparks`: 50 per galaxy

## CLI

The same aggregation drives `cs motion`. It reads the filesystem
locally — no HTTP, no loopback dependency on `cs-api`.

```sh
cs motion                           # ANSI table (five sections, colored)
cs motion --watch                   # redraws every 3 s (Ctrl-C to quit)
cs motion --json                    # agent-first NDJSON object
cs motion --window 1h               # widen the "recent" window
cs motion --galaxies cosmon,mailroom
cs motion --include workers,molecules
```

## Pilots

Both the Mac pilot (popover, 5th tab, ⌘5) and the iOS pilot (TabView)
expose Motion as their own tab with collapsible sections. They poll
every 3 seconds while visible and pause when dismissed. Colored dots
signal worker health (green/yellow/red/gray); sections render empty
"—" placeholders so the surface never hides a missing section behind
a silent skip.

## Scope-guards

- **Pull-only v0.** No webhooks. The v1 polling cadence is 3 s across
  all three surfaces. A future WebSocket push is deferred until the
  operator asks for it.
- **No daemon** (ADR-016). `cs motion` and `cs-api /motion` do the
  same filesystem scan per invocation.
- **Cost fields are reserved.** `workers[].cost_usd`,
  `input_tokens`, `output_tokens` are null today. Wiring claudion's
  session-energy probe is tracked as a follow-up task — the schema is
  stable in the meantime.
- **Git scan stays local.** The aggregation runs `git log --since`
  at the galaxy root; no clone, no fetch, no network.
- **Respects the path invariants.** `cs-api --cosmon-state` and
  `--galaxies-root` resolve the same roots as `/inbox` and
  `/galaxies`; `cs motion` walks up from `$CWD`.

## Where this lives in the code

| Surface | File |
|---------|------|
| HTTP | `crates/cosmon-api/src/motion.rs` (public `aggregate_motion`) |
| CLI | `crates/cosmon-cli/src/cmd/motion.rs` |
| Mac pilot | `apps/mac-pilot/mac-pilot/MotionView.swift` + `CosmonBridge.motion()` |
| iOS pilot | `apps/ios-pilot/ios-pilot/MotionView.swift` + `CosmonAPI.motion()` |

## See also

- `docs/guides/cs-api.md` — the endpoint table cs-api serves.
- An internal chronicle — the image-pivot chronicle:
  *"la grande salle de contrôle avec les écrans qui montrent quels
  trains roulent, où, et à quelle vitesse."*
