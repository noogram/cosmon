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
