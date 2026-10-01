# Protect reference inputs from a worker

**Goal:** give a worker ground truth to read (expected outputs, golden files, a
reference dataset) and make sure the merge fails if the worker changed it.

A worker that is told to make a result match its reference can do so by editing
the reference. `--protect` closes that route.

## Declare the paths when you nucleate

```sh
cs nucleate task-work --var topic="Make the parser pass the fixtures" \
    --protect tests/fixtures/expected.json \
    --protect data/reference/
```

Each `--protect` is relative to the repository root and names a file or a
directory (a directory protects everything below it). It is repeatable. An
absolute path or one containing `..` is refused before any molecule is created,
and the flag cannot be combined with `--from`.

## What happens next

- The worker's briefing lists the paths as read-only, with the reason.
- `cs tackle` clears the write bits on those paths in the worker's worktree.
- `cs done` refuses a branch that changed any of them. It exits with code 78
  (`protected_path_modified`) and names each path.

## When the change is intended

If you have read the change and it is correct, merge it explicitly:

```sh
cs done <id> --allow-protected-change
```

The override exists only at the terminal; the Remote Pilot Port has no
equivalent.

See also: [Molecule lifecycle commands](../reference/lifecycle.md) for the
`--protect` flag, and [Execution commands](../reference/execution.md) for
`cs done`.
