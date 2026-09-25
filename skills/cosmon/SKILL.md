---
name: cosmon
description: Pilot cosmon (the `cs` CLI) by natural language — nucleate, tackle, watch, correct, and merge a unit of work. Use when the user wants to delegate a task to an AI agent through cosmon, or mentions `cs`, nucleate, tackle, molecule, whisper, or a cosmon project.
---

# Cosmon

Run `cs help` for the full command reference, `cs help guide` for the operator handbook, and `man cs` for the manual page.

Source of truth: `.cosmon/state/` (JSON). Surfaces are projections — never edit directly.

The normal cycle for one unit of work:
```
cs nucleate task-work --var-file topic=<file>   # a long statement goes in a file, not a flag
cs tackle <id> --adapter claude                 # spawn a worker; omit --adapter for the project default
cs peek                                         # watch it work, or `cs wait <id>` to block
cs whisper <id> --file correction.md            # send a correction while the worker is still open
cs done <id>                                    # merge to the base branch + teardown (required)
```
Use `cs tackle`, not `cs run`, when you intend to read the result before merging: `cs run` walks a whole DAG and calls `cs done` on completion itself, closing the review window.
