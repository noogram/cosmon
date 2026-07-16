# Diagnostic — `cs peek` reported killed in `earshot` repo

**Molecule:** `task-20260414-f236`
**Date:** 2026-04-14
**Reporter:** operator (Noogram)
**Symptom (verbatim):** running `cs peek` in `/Users/you/dev/YOU/earshot`
terminates with shell message `[1] <pid> killed cs peek`. Works fine in
`cosmon` and `foundry` repos.

---

## TL;DR

The SIGKILL pattern could **not be reproduced** in a controlled PTY.
`cs peek` runs cleanly in `earshot` under every synthetic repro I attempted.
The `[1] <pid> killed` shell output almost certainly comes from zsh
job-control reporting on a **backgrounded or suspended `cs peek` that received
SIGKILL from outside the process** (most plausible: the user Ctrl-Z'd a stuck
instance, or a stale cs peek from a previous session got swept by a `pkill`).

However, the investigation surfaced two real robustness gaps that would cause
`cs peek` to leave the user's terminal in a bad state if the TUI ever did
crash, and to produce a cryptic errno if stdout is not a TTY. Both are fixed
in this molecule.

**Fix landed on this branch:**

1. **Panic-safe terminal restoration** — `run()` installs a scoped panic hook
   that disables raw mode and leaves the alternate screen before the default
   hook prints the backtrace. Previously, a panic anywhere in `event_loop`
   would leave the terminal stuck in raw mode — the user's shell prompt would
   render garbled and `stty sane` would be the only recovery.
2. **TTY preflight in `setup_terminal`** — fails fast with an actionable
   message when stdout is not a terminal. Previously, running `cs peek` in a
   non-TTY context produced the cryptic `Device not configured (os error 6)`
   from deep inside crossterm's `enable_raw_mode` — the same errno that the
   user may have been seeing fragments of. Regression test added
   (`setup_terminal_refuses_without_tty`).

Both changes live in `crates/cosmon-cli/src/cmd/peek_tui/mod.rs` and cost
~40 lines.

---

## Evidence

### Reproduction attempts

**Attempt 1 — non-TTY (this claude-code shell):**

```
$ cd ~/dev/YOU/earshot && RUST_BACKTRACE=full cs peek 2> /tmp/peek-err.log
$ echo EXIT=$?                        → EXIT=1
$ cat /tmp/peek-err.log               → cs: Device not configured (os error 6)
```

Clean, graceful error exit — no SIGKILL. The `Device not configured` is
crossterm's error bubbling up from `enable_raw_mode`, returned as an anyhow
error by the entrypoint. Exit code 1, no signal.

**Attempt 2 — TTY via detached tmux:**

```
$ tmux new-session -d -s peek-repro -x 200 -y 50 \
    "cd ~/dev/YOU/earshot && RUST_BACKTRACE=full cs peek 2>/tmp/peek-tty-err.log"
$ sleep 5; tmux capture-pane -t peek-repro -p | head
```

TUI rendered normally. Showed 163 molecules, workers, heartbeats. No kill,
no panic. Both `cs peek` (default) and `cs peek --all` tested. All clean.

**Attempt 3 — concurrency stress (8 concurrent TUI instances in earshot):**

```
$ for i in 1..8; do tmux new-session -d -s peek-test-$i "cs peek …"; done
$ sleep 6
$ for i in 1..8; do tmux list-panes -t peek-test-$i -F '#{pane_dead}'; done
# → every session: dead=0
```

All 8 concurrent TUI instances survived. No contention kills.

**Attempt 4 — check for jetsam / kernel kill logs:**

```
$ log show --predicate 'eventMessage contains "jetsam"' --last 6h | grep "cs "
$ log show --predicate 'processImagePath contains "cs"' --last 12h \
    | grep -iE "killed|terminated|memorypressure"
# → empty
```

macOS unified log has no record of any `cs peek` being killed by jetsam,
kernel, or memory pressure in the last 12 hours.

### Memory & resource sanity

| measurement | value |
|---|---|
| `cs peek` RSS (earshot, fully loaded TUI) | 46 MB |
| `cs peek` VSZ | ~412 MB (virtual, normal for a tokio-less TUI binary) |
| earshot `.cosmon/state/` total | 2.9 MB |
| molecules | 163 |
| `events.jsonl` | 222 KB |
| `fleet.json` | 8.4 KB |
| average `state.json` per molecule | ~3 KB |

Nothing pathological. The state is perfectly normal-sized.

### Process state at investigation time

```
$ ps -ax -o pid,etime,rss,state,command -p $(pgrep -f '^cs peek$')
    PID     ELAPSED   RSS STAT COMMAND
  56134    07:53:24  18M  S+   cs peek
  19052    06:05:41  46M  S+   cs peek
  70111    08:25:25  27M  S+   cs peek
  53337 01-06:05:54  11M  S+   cs peek
  39842       28:16  16M  S+   cs peek
```

**Five active `cs peek` instances, one running over a day**, all in `S+`
(interruptible sleep, foreground process group). None have died. The operator
clearly runs `cs peek` heavily and it stays alive in normal use.

### What succeeded where a kill might have been expected

- `cs peek --no-tui --once` — printed baseline + fleet diff cleanly.
  Rules out a corrupt `state.json` or malformed `fleet.json` triggering a
  panic during load.
- `cs peek --all` in a PTY — rendered 1055 molecules across 19 projects
  without issue.

### Code audit highlights

- No `unsafe` blocks in `crates/cosmon-cli/src/cmd/peek_tui/`.
- No `unwrap()` / `panic!()` / `unreachable!()` in the TUI hot path.
- Raw-mode lifetime is bracketed: `setup_terminal → event_loop →
  restore_terminal`. **But:** if `event_loop` panicked, `restore_terminal`
  was never reached. **(Gap #1 — now fixed.)**
- `setup_terminal` called `enable_raw_mode()` unconditionally, with no
  preflight for stdout being a TTY. When invoked via a pipe, it surfaced
  `Device not configured (os error 6)` from deep inside crossterm.
  **(Gap #2 — now fixed.)**

---

## Root-cause hypothesis (unconfirmed)

The strongest hypothesis given the `[1] <pid> killed cs peek` shell syntax
— which zsh uses for **job-control status changes**, not for foreground
exits — is:

> The operator either backgrounded a `cs peek` instance (`^Z` + `bg`, or
> started with `&`) or started it in a subshell that was later backgrounded.
> That instance then received SIGKILL from outside the process, and zsh
> printed the `[1] <pid> killed` line when it next checked job status
> (typically at the next prompt redraw).

Why this fits the evidence:

- The shell prefix `[N]` only appears for jobs zsh is actively tracking
  in its job table. A plain foreground SIGKILL prints `zsh: killed <cmd>`,
  **not** `[N] <pid> killed`.
- Five concurrent `cs peek` processes were found running at investigation
  time, including one over a day old — strong evidence that the operator
  starts many and doesn't always clean them up.
- No kernel logs recorded a jetsam kill, so the kill came from userspace
  (a `kill -9`, `pkill cs peek`, `killall cs`, or an ancestor session
  going away with SIGHUP → something escalating).

Why earshot specifically? Probably coincidence — earshot is the most
active project in the fleet (163 molecules, 21 workers), so it's the
repo where `cs peek` is run most often, and therefore the repo where
stray backgrounded instances accumulate.

---

## Fix landed

### 1. Panic-safe terminal restoration

Scoped panic hook inside `run()`:

```rust
let prev_hook = std::panic::take_hook();
std::panic::set_hook(Box::new(|info| {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    eprintln!("cs peek panicked: {info}");
}));
let res = app.event_loop(&mut terminal);
std::panic::set_hook(prev_hook);
```

The hook is **scoped** — we save the previous hook, install ours for the
duration of the event loop, and restore it. Two benefits:

- Tests and later code in the same process see the hook they expect.
- The hook only restores the terminal when we own it.

This does not stop a SIGKILL (impossible — SIGKILL is not catchable), but
it ensures that if `cs peek` ever does panic (future bug), the user's
terminal returns to a sane state.

### 2. TTY preflight

```rust
if !io::stdout().is_terminal() {
    return Err(anyhow::anyhow!(
        "cs peek requires a TTY on stdout; got a pipe or redirected stream. \
         Run `cs peek --no-tui` for a non-interactive view, `cs peek --json` \
         for scripting, or `cs peek --snapshot` for a fixed-width capture."
    ));
}
```

Runs **before** `enable_raw_mode()`, so it cannot leave the TTY in raw mode.
Replaces the cryptic `Device not configured (os error 6)` with an actionable
message pointing the user at the correct flag.

### Regression test

`setup_terminal_refuses_without_tty` in the `tests` module: under
`cargo test`, stdout is captured (not a TTY), so calling `setup_terminal()`
must return the preflight error. The test pins the observable behavior so
any future regression (removing the preflight, changing the error message)
is caught by CI.

---

## What this diagnostic does NOT fix

- The actual SIGKILL source. Without a reproducer, we can't patch the
  path. If it recurs, the next diagnostic should:
  1. Run `cs peek` in the misbehaving terminal with `strace -f -e trace=signal`
     (or `sudo dtrace -n 'proc:::signal-send { printf("%d -> %d sig %d\n", pid, args[1]->pr_pid, args[2]); }'` on macOS).
  2. Note whether the user had pressed Ctrl-Z recently.
  3. Check `jobs -l` in the shell right before `cs peek` is started.
- A leak of old `cs peek` instances. Five concurrent running processes
  is high; a future improvement could have `cs peek` acquire a TTY-scoped
  advisory lock and refuse to start if another is already attached to
  the same TTY — but that is a separate molecule.

---

## Recovery recipe (for the operator, next time this happens)

If `cs peek` appears to hang or gets killed:

```zsh
# 1. List stray cs peek processes.
pgrep -af 'cs peek'

# 2. Check if zsh has a job in its table.
jobs -l

# 3. If the terminal renders garbled (post-crash), reset it:
stty sane && clear

# 4. Kill all stray cs peek instances (safe — they're stateless readers):
pkill -x 'cs peek' || killall cs

# 5. Start a fresh one:
cs peek
```

---

## Chronicle candidate?

**Not yet.** The principle this illuminates — "a TUI must survive a panic
without corrupting the operator's terminal" — is sound but not unique to
cosmon. It's standard crossterm hygiene. If the SIGKILL ever gets a real
reproducer and the root cause turns out to be architecturally instructive
(e.g. two cosmon processes fighting for the same TTY because of a missing
singleton discipline), chronicle it then.

---

## Files touched

- `crates/cosmon-cli/src/cmd/peek_tui/mod.rs` — panic hook, TTY preflight,
  regression test.
- `docs/diagnostics/cs-peek-earshot-kill.md` — this document.
