# Install the Resident Runtime — one TOML block, then flip a bit

> *Le runtime résident est un petit moteur qui fait tourner cosmon tout
> seul pendant que tu fais autre chose. On l'allume comme on allume un
> chauffe-eau : on ajoute son nom à la liste des appareils branchés,
> puis on bascule l'interrupteur sur ON quand on est prêt.*

**Audience:** an operator who has read
[ADR-095](../adr/095-resident-runtime-ifbdd-path.md), understands the
IFBDD opt-in pact, and now wants the Resident Runtime alive on a
local machine so the DAG advances without an external `cs tick`.

**Governing ADRs:**

- [ADR-095](../adr/095-resident-runtime-ifbdd-path.md) — Resident
  Runtime ratified under five structural invariants and a 90-day
  forensic evaluation gate. §5 binds the install canal.
- [ADR-053](../adr/053-cosmon-daemon-supervisor.md) — the
  daemon-supervisor is the *canal*. One TOML, one supervisor, N
  declared daemons. Hot-reloads on file save.

## The model in one image

The supervisor is a power strip. `~/.config/cosmon/daemons.toml` is
the list of things plugged into the strip. Each `[[daemon]]` block is
one socket. `enabled = false` is "plugged in, switch off". `enabled =
true` is "plugged in, switch on". Saving the file is the operator
lifting the switch. The supervisor sees the save, hot-reloads, starts
(or stops) the child. No `launchctl`, no copy, no `sed`.

## Pre-requisites

1. **`cs` on `$PATH`.** The supervisor will `exec` it.
   Verify with `which cs && cs --version`.
2. **`cosmon-daemon-supervisor` installed.** Verify with
   `cs daemons list` (it must respond, even with no daemons running).
   If not installed, run `scripts/install-daemon-supervisor.sh install`
   from the cosmon repo first.
3. **The forensic instrument lives.** RR-5 events in `events.jsonl`
   are wired before the loop ships. Verify with
   `cs events --since '24 hours ago' --kind Runtime | head` — no
   error is sufficient (the absence of events is normal until the
   runtime runs).
4. **`~/.cosmon/logs/` exists.** `mkdir -p ~/.cosmon/logs` if not.

## The canonical `[[daemon]]` block

Append this block to `~/.config/cosmon/daemons.toml`, in the same
style as the existing `notification-bot`, `incredibles-bot`,
`notification-bot` blocks. Keep `enabled = false` on the first save —
that is the IFBDD opt-in gesture in concrete form.

```toml
# cosmon-runtime — ADR-095 Resident Runtime.
#
# A long-running `cs run --resident` loop. Reads .cosmon/state/, walks
# the DAG, calls cs tackle / cs evolve / cs done on ready molecules
# the same way a human would from a sibling shell (RR-1: client of
# the transactional core; RR-2: owns no state). The trace lives at
# .cosmon/state/runtime-trace.jsonl (RR-5).
#
# IFBDD opt-in pact: ship with enabled = false. Flip to true when
# ready to start the 90-day forensic measurement window (ADR-095 §4).
# Flip back to false (or `touch` the kill_switch) to stop the runtime
# without removing the declaration.
#
# `--config <state-dir>` pins the runtime to *this* state store
# regardless of the supervisor's cwd. The runtime walks two levels up
# from the state-dir to derive the project root used as cwd for child
# `cs` calls (see crates/cosmon-cli/src/cmd/run.rs::run_resident).
[[daemon]]
name = "cosmon-runtime"
binary = "/Users/you/.local/bin/cs"
args = ["--config", "/srv/cosmon/cosmon/.cosmon/state", "run", "--resident", "--poll-interval", "5"]
env = { HOME = "/Users/you", PATH = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/Users/you/.local/bin", RUST_LOG = "info" }
log_stdout = "/Users/you/.cosmon/logs/runtime.log"
log_stderr = "/Users/you/.cosmon/logs/runtime.err"
enabled = false
kill_switch = "/Users/you/.cosmon/runtime-stand-down.lock"
throttle_seconds = 30
```

Adjust `binary`, the `--config` path, `env.HOME`, and the log paths
if your layout differs from `/Users/you/…`. The `--config` value
must end in `.cosmon/state` — that is the directory containing
`fleets/`, `presence/`, and `events.jsonl`.

## The five gestures

### 1. Add the block (one-time)

Open `~/.config/cosmon/daemons.toml`, paste the block above at the
bottom, save. The supervisor hot-reloads; because `enabled = false`,
nothing starts. Verify with `cs daemons list` — `cosmon-runtime`
should appear with state `disabled`.

### 2. Activate (the IFBDD flip)

Edit the same block, change `enabled = false` to `enabled = true`,
save. The supervisor sees the file event, starts the child within
the debounce window (~200 ms), `cs daemons list` now shows `running`
with a pid. The 90-day forensic gate starts ticking *now*.

### 3. Observe

- **State summary:** `cs daemons list`.
- **Live log:** `tail -F ~/.cosmon/logs/runtime.log`.
- **NDJSON trace (RR-5):** `tail -F .cosmon/state/runtime-trace.jsonl`
  from the working directory.
- **Forensic events:** `cs events --kind RuntimeReadDecideWrite`,
  `--kind RuntimeShelledOut`, `--kind RuntimeMergeDispatched`,
  `--kind RuntimeWorktreeClaimed`.

### 4. Stand down (temporary)

Two paths, same effect:

- **Edit:** flip `enabled = true` → `false`, save. Supervisor SIGTERMs
  the child within ~200 ms. Declarative; survives reboots.
- **Lockfile:** `touch ~/.cosmon/runtime-stand-down.lock`. Supervisor
  honors the per-daemon kill-switch immediately. Imperative; ephemeral
  if the file is removed.

Use the *edit* path for "I'm done self-application for the week". Use the
*lockfile* path for "stop now, I'll re-enable from a remote shell in
a few minutes".

### 5. Excise (RR-3 — the deletion path)

If the 90-day forensic evaluation comes back **build-falsified**
(ADR-095 §4), the runtime is excised. The install reversal is one
file edit: delete the `[[daemon]]` block from
`~/.config/cosmon/daemons.toml`, save. The supervisor reloads, the
child is gone. (The crate excision — `cosmon-runtime` itself — is a
separate one-PR operation under RR-3; this guide covers only the
operator-side install reversal.)

## Why not a standalone LaunchAgent?

The first build wave shipped
`packaging/launchd/com.noogram.cosmon.runtime.plist` with a
`launchctl bootstrap` + `sed` install ritual. It was retired by
`task-20260518-b420` because:

- The `com.noogram.*` namespace does not exist on the operator's
  machine — cosmon-orbit LaunchAgents are `com.cosmon.*`
  (`com.cosmon.scheduler`, `com.cosmon.daemon-supervisor`).
- Throttle / kill-switch / log discipline was being re-invented per
  daemon. The supervisor already owns it (ADR-053 §9).
- "One concept, infinite extensibility" (CLAUDE.md §Composability):
  adding a cosmon-orbit daemon must cost one TOML entry, not one
  bespoke install script.

The deprecated template is preserved at
`scripts/launchd/archived/com.noogram.cosmon.runtime.plist.deprecated`
as a historical witness — do not install it.

## Reboot survival — inherited, not bolted on

The runtime does **not** get its own LaunchAgent. Two restart
authorities for one process is a double-spawn bug: launchd respawns
the runtime *and* the supervisor respawns it, and they race. The
runtime survives reboots **transitively**, through the one authority
that owns it:

```
launchd ──(KeepAlive=true, RunAtLoad=true)──▶ com.cosmon.daemon-supervisor
                                                        │
                              reads ~/.config/cosmon/daemons.toml
                                                        │
                              spawns + KeepAlive ──▶ cosmon-runtime  (enabled = true)
                                                  └─▶ cosmon-scheduler, tg-bot, …
```

On reboot launchd starts the supervisor (`RunAtLoad`), the supervisor
reads `daemons.toml`, and every `enabled = true` child — the runtime
included — comes back. There is exactly one plist on the machine for
this whole tree:
[`scripts/launchd/com.cosmon.daemon-supervisor.plist`](../../scripts/launchd/com.cosmon.daemon-supervisor.plist)
(`KeepAlive = true`, `RunAtLoad = true`, `ThrottleInterval = 5`).

**Verify the chain without rebooting:**

```sh
scripts/soak/reboot-survival-check.sh
```

It asserts the supervisor plist carries `KeepAlive` + `RunAtLoad`,
that the supervisor process is alive, and that each `enabled = true`
daemon has a live pid — the same end-state a reboot must re-establish.
For the real thing, the script prints the manual reboot soak
procedure (reboot → wait → re-run the check). See
[task-20260608-1c59](../../.cosmon/state/fleets/default/molecules/task-20260608-1c59/)
for the rationale (the runtime was "dead all night" partly because
nobody had promoted reboot-survival from a claim to a check).

## The self-hosting loop is a different thing

`just self-runtime` (from the cosmon repo root) is the *workshop*
form: foreground process, `tail -F` on the trace, Ctrl-C to stop.
It does not declare a daemon. Use it when you want to *watch* the
runtime make a decision; use the supervisor block above when you
want the runtime to *live* on the machine.

## Troubleshooting

- **`cs daemons list` does not show `cosmon-runtime`.** The supervisor
  has not reloaded. Check it is running: `cs daemons supervisor-status`
  (or `launchctl list | grep cosmon`). If unloaded, run
  `scripts/install-daemon-supervisor.sh install`.
- **State `crashed` with rapid respawn.** The `throttle_seconds = 30`
  floor should dampen the loop. If the supervisor is suppressing
  spawns, check `~/.cosmon/daemon-supervisor.log` for the crash
  reason. Common causes: `binary` path wrong, `cs` missing from
  `$PATH` in the daemon's `env`, `.cosmon/state/` not writable from
  the daemon's `working_directory`.
- **`PropulsionDown` alert fired (task-20260608-1c59).** When a
  supervised child crash-loops — by default **5 crash-restarts inside
  300 s** (`[supervisor] crash_loop_threshold` /
  `crash_loop_window_seconds`) — the supervisor stops failing in
  silence and surfaces one operator-visible `PropulsionDown` alert via
  `cs notify` (`[supervisor] notify_command`, default `["cs",
  "notify"]`). This is the escape valve for the kahneman crack: a
  config that *parses* but is semantically broken would otherwise
  re-spawn forever with no signal. The alert names the daemon and the
  count; treat it as "the runtime will not stay up, look now" rather
  than a transient. The valve re-arms automatically once the child
  stops crashing for a full window. Set `crash_loop_threshold = 0` to
  disable it.
- **Runtime starts but does nothing.** That is the expected idle
  state when no DAG has ready molecules. Confirm with
  `tail -F .cosmon/state/runtime-trace.jsonl` — you should see one
  `tick` line per `--poll-interval 5` window with `action = "idle"`.

## See also

- [ADR-053](../adr/053-cosmon-daemon-supervisor.md) §9 — Resident
  Runtime is now one of the supervised daemons (cross-reference).
- [ADR-095](../adr/095-resident-runtime-ifbdd-path.md) §5 — install
  canal binding.
- [`docs/handbook.md`](../handbook.md) §Channels — where the runtime
  sits in the six-channel model.
- [`scripts/launchd/archived/com.noogram.cosmon.runtime.plist.deprecated`](../../scripts/launchd/archived/com.noogram.cosmon.runtime.plist.deprecated)
  — historical witness, do not install.
