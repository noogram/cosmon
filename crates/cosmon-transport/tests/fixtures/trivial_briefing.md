# Trivial cross-Adapter smoke briefing (ADR-098 / C8)

Write a file at `$MOLECULE_DIR/output.md` containing the line:

    hello from <YOUR-ADAPTER-NAME>

(Replace `<YOUR-ADAPTER-NAME>` with `claude` or `aider` per the Adapter
that runs this briefing.)

Then call `cs evolve` to advance past this step. No code is required —
just the file write and the advance.

This briefing exercises all four obligations of ADR-079 §5:

1. **Briefing read** — the Adapter must read this file.
2. **Writable `MOLECULE_DIR`** — the Adapter must write `output.md`
   under `$MOLECULE_DIR`.
3. **`cs` on PATH in the worktree** — the Adapter must run `cs evolve`.
4. **Idempotent termination** — `cs complete` or worker exit closes
   the session.
