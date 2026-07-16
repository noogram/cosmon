# fake-tmux

Minimal bash reimplementation of the `tmux(1)` subset that cosmon actually
calls, plus env-var fault injection. The goal is not a correct tmux — it is
a **boundary oracle** for the process-semantics contract cosmon depends on.

## Surface covered

| command       | why cosmon calls it                               |
|---------------|---------------------------------------------------|
| `new-session` | `cs tackle` spawns the worker process             |
| `list-sessions` | `cs peek`, `cs observe`, readiness polling      |
| `list-panes`  | readiness (`pane_dead`), whisper (`pane_current_command`), energy probe (`pane_pid`) |
| `kill-session` | `cs done` + failure recovery                     |
| `kill-server` | test cleanup                                      |
| `has-session` | liveness check in runtime                         |
| `capture-pane` | `cs peek` tab switching, watchdog diagnostics    |
| `send-keys`, `load-buffer`, `paste-buffer`, `delete-buffer` | prompt injection (`send_input`) |
| `set-hook`, `wait-for` | graceful exit plumbing                    |

## Fault injection

| env var                       | effect                                         |
|------------------------------|------------------------------------------------|
| `FAKE_TMUX_NEW_SESSION_EXIT` | exit code for `new-session` (default `0`)      |
| `FAKE_TMUX_PANE_DEAD=1`      | `list-panes` reports `pane_dead=1`              |
| `FAKE_TMUX_SESSION_EXITED=1` | session exists but pane is dead + PID zeroed    |
| `FAKE_TMUX_LIST_EMPTY=1`     | `list-sessions` returns empty                   |
| `FAKE_TMUX_HAS_SESSION_MISS=1` | `has-session` always exits 1 (not found)     |
| `FAKE_TMUX_NO_SERVER=1`      | `list-sessions` reports "no server running"     |
| `FAKE_TMUX_PANE_CMD=<name>`  | override `pane_current_command` (default `claude`) |
| `FAKE_TMUX_FORK=1`           | actually fork the run command into the background |
| `FAKE_TMUX_TRACE=<path>`     | log every call into `<path>` for debugging      |
| `FAKE_TMUX_DIR=<path>`       | state directory (default `$TMPDIR/fake-tmux`)   |

## Non-goals

- Not a drop-in replacement. Unknown commands print a warning and exit 0
  (so a stray `tmux display-message` doesn't abort the harness).
- Not transactional. Two concurrent `new-session` on the same socket race
  on the metadata file (matches real tmux's behavior well enough).
- No actual pane rendering. `capture-pane` returns whatever `send-keys` /
  `paste-buffer` appended — good enough to verify prompt injection landed.

## Usage

Make the script executable and put its directory first on `PATH`:

```bash
chmod +x tests/fakes/fake-tmux/tmux
export PATH="$(pwd)/tests/fakes/fake-tmux:$PATH"
export FAKE_TMUX_DIR="$(mktemp -d)"

tmux -L my-socket new-session -d -s w1 sleep 60
tmux -L my-socket list-sessions -F '#{session_name}'   # → w1
```

See `tests/harness/run_matrix.sh` for the full integration pattern.
