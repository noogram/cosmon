# fake-claude

Drop-in replacement for the `claude` CLI used by cosmon's `spawn_claude`
path. Ships with a fixed catalog of failure-mode behaviors controllable
via `FAKE_CLAUDE_MODE`.

## Modes

| mode             | behavior                                            |
|------------------|-----------------------------------------------------|
| `exit-0`         | Exit cleanly (default)                              |
| `exit-42`        | Immediate exit code 42                              |
| `exit-delayed`   | Sleep 2s, exit 1 (mid-spawn death)                  |
| `hang`           | Infinite sleep loop (frozen worker)                 |
| `segfault`       | `kill -SEGV $$` — simulate native crash (exit 139)  |
| `auth-prompt`    | Print auth error to stderr, exit 1                  |
| `partial-output` | Emit a few NDJSON event lines, then exit 1          |
| `echo-prompt`    | Echo argv and stdin, exit 0 (visibility aid)        |
| `complete-molecule` | Read the briefing off stdin, `cs complete <id>`  |

## `complete-molecule`

The only mode that *succeeds at the job* rather than reproducing a way of
failing at it. It reads the briefing the transport port pastes into the
pane, takes the first cosmon molecule id it finds, and runs
`cs complete <id>`. That makes it the smallest worker a dispatch can end
with a completed molecule, which is what the container e2e
(`scripts/rpp-remote-e2e.sh`) needs: the property under test there is that
the rpp-adapter — with no `cs` binary of its own since issue #54 U6 —
really spawns a worker that can drive `cs` against the tenant store.

Before completing, it writes one file in its worktree and commits it. A
worker that finishes with no commit leaves a branch with nothing on it,
and `cs done` then archives that branch instead of merging it — a
legitimate outcome, and one the container e2e's merge assertion cannot be
written against. `FAKE_CLAUDE_NO_COMMIT=1` restores the empty-branch shape
for a test that wants it. Identity comes from the repository config, not
from `$HOME`: the worker envelope's home holds no git identity.

It needs `cs` and `git` on `PATH` and a resolvable state dir; inside the
e2e image the adapter's worker envelope pins `COSMON_STATE_DIR`. Knobs:
`FAKE_CLAUDE_READ_TIMEOUT` (seconds of silence before it gives up, default
120 — a briefing that never arrives must end as a named failure, not a
pane that idles forever), `FAKE_CLAUDE_DONE_FILE` (a breadcrumb file
written with the id it acted on), and `FAKE_CLAUDE_NO_COMMIT` (skip the worker's own commit), `FAKE_CLAUDE_LINGER` (seconds to
stay alive after completing, default 120).

That last one is not padding. A briefing is delivered as `load-buffer` →
`paste-buffer` → a trailing submit key, so a worker that exits the instant
it reads the paste takes its tmux session — and, when it is the only one,
the tmux server — down *between* the paste and the submit. The submit then
fails and the dispatch is reported as a spawn failure, with its ledger
entry and worktree rolled back under a molecule that is already
`completed`. A real agent never disappears mid-briefing; neither does
this.

## Debug

- `PROMPT_ECHO=1` + `FAKE_CLAUDE_LOG=<path>` — capture argv and stdin for
  inspection.

## Argv handling

Unknown flags are ignored. Flags cosmon invokes (`--permission-mode`,
`--dangerously-skip-permissions`, `--model`, `--print`) are explicitly
consumed to keep positional parsing clean.

## Usage

Make it executable and stage ahead of the real `claude` on `PATH`:

```bash
chmod +x tests/fakes/fake-claude/claude
export PATH="$(pwd)/tests/fakes/fake-claude:$PATH"
export FAKE_CLAUDE_MODE=exit-42

claude --permission-mode bypassPermissions   # exits 42
```

See `tests/harness/run_matrix.sh` for the full matrix.
