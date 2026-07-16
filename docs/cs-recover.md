# Crash recovery — the real verbs

> **Drift fix (2026-07-11, D1).** Earlier revisions of this page documented a
> `cs recover` command with an implementation at
> `crates/cosmon-cli/src/cmd/recover.rs`. **Neither ever existed** — there is no
> `Recover` variant on the `Command` enum and no `recover.rs`. The page below
> repoints to the recovery verbs that actually ship. It is the seed for the
> future `how-to/recover-crashed-agent.md` (B1′ plan, P4).

Crash recovery in cosmon is **stateless and project-local**: state lives on disk
(`.cosmon/state/`), not in RAM, so a dead tmux server, an OOM kill, or a host
reboot never loses a molecule — it only *strands* the live worker that was
driving it. Recovery is the act of noticing the strand and re-attaching (or
retiring) a fresh observer. No single command does all of it; the flow is a
short sequence of one-decision-per-invocation verbs.

## The verbs (all shipping, sourced from the CLI)

| Verb | Role |
|------|------|
| `cs patrol` | Patrol the fleet — health checks and anomaly detection. **This is the scan** for stranded molecules (lifecycle says `Running`, tmux is dead). |
| `cs health` | Read-only molecule-health anomaly catalog, federation-wide (ADR-137 §7). A no-mutation view of the same signals `cs patrol` acts on. |
| `cs resurrect <id>` | Revive a **wrecked (stuck)** molecule with a fresh worker. The re-tackle after a crash. |
| `cs resume <id>` | Convenience alias for `cs patrol --propel --molecule <id>` — re-propel a live-but-idle worker. |
| `cs stuck <id>` | Freeze a molecule and record the blocker (the manual "mark stranded" gesture). |
| `cs thaw <id>` | Resume a frozen worker's Claude session. |
| `cs collapse <id>` | Terminate a molecule with final-state recording, when recovery is not wanted. |

## When to run

- After a host reboot or IDE crash.
- After a `SIGKILL` / OOM of a tmux server.
- When `cs ensemble` shows `Running` molecules that `cs peek` cannot attach to.
- As the first step of any "something is wrong, I don't know what" triage,
  before `cs collapse`.

## The flow

1. **Scan.** `cs patrol` (or `cs health` for a read-only look) surfaces
   molecules whose lifecycle says `Running` but whose worker is gone.
2. **Decide, one molecule at a time.** For each stranded molecule:
   - live worker just idle → `cs resume <id>` (re-propel);
   - worker genuinely dead, wreck recoverable → `cs resurrect <id>`
     (fresh worker);
   - not worth recovering → `cs collapse <id>` (terminal, with reason).

`cs patrol` **never auto-re-tackles** silently — it detects and, with
`--propel`, nudges. A human reads the verdict and picks the verb. This mirrors
the command perimeters in
[architectural-invariants.md](architectural-invariants.md) §3.

## Scope boundaries

- **In scope.** Molecules on this fleet, in this project (walk-up discovery of
  `.cosmon/state/`).
- **Out of scope.** Other fleets (run the recovery verbs in each project dir);
  `Pending` / `Completed` / `Collapsed` molecules (those are not stranded).

## See also

- [ADR-016 — autonomy regimes](adr/016-autonomy-regimes-and-resident-runtime.md)
- [architectural-invariants §3 — command perimeters](architectural-invariants.md)
- [`cosmon-crashtest`](../crates/cosmon-crashtest/README.md) — bisimulation harness
