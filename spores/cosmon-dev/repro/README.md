# The red repro-contracts (`cosmon-dev` §5 — the seeds)

> Three deterministic red reproductions for the v0.2.2 env-fit issues. Each fails
> on `affected_ref` for the RIGHT reason, is frozen before any fix, ships a
> differential refutation, and names its false-green / false-red modes. They are
> the concrete instances the `clean-room-repro` gate (G2) produces and freezes.

## Why these live here and NOT in the cosmon workspace test suite

Each contract is **red by design on the parent ref** (v0.2.2). Wiring a
by-design-red test into `cargo test --workspace` would redden the repo's own green
gates for a test that is *supposed* to fail on the old code. So the contracts live
under `repro/` as standalone harnesses the clean-room runs against a
`git archive <affected_ref>` checkout (see `../clean-room/`). When a fix lands, the
same frozen harness goes green — that is the G5 witness. The mission's own gates
(`cargo check/test/clippy/fmt/doc` on the fix branch) stay green throughout.

## The three seeds

| file | issue | class | LLM needed? |
|------|-------|-------|-------------|
| `contract-21-adapter-resolver.md` | #21 | resolver precedence under `--resident` | NO (pure resolver assertion) |
| `contract-20A-root-bypass-spawn.md` | #20A | root-run spawns a live worker on the dead root path | NO (uid + process assertion) |
| `contract-20B-prompt-write-outside-worktree.md` | #20B | prompts + state written outside the worktree | offline `claude -p` (model neutralised) |

## The shared discipline (every contract obeys)

1. **Right reason, not merely red.** The failure message must match the *contract
   breach* (blueprint G1), not an unrelated panic. A red for the wrong reason is a
   false-red; the harness names how to tell them apart.
2. **Assert on the pre-oracle decision, never on model output.** #21 asserts on the
   COMPOSED resolver `resolve_adapter_selection(...)` (surfaced via the
   `adapter_selected` event's `selection_source`), NOT on the literal `--adapter`
   string a code path emits. #20B's primary assertion is a positive sentinel file
   written via a nonce known only to the prompt; the timeout (rc=124) is a
   *secondary* liveness guard only.
3. **Differential refutation.** Flip exactly ONE variable (the fix) and the colour
   BASCULES; revert and it returns red. A colour that does not move is a tautology.
4. **False-green + false-red named.** Each contract states how it could wrongly
   pass while the bug persists, and how it could wrongly fail while the bug is
   absent. Each named mode is a guard.
5. **No skip-if-not-root for a root bug** (#20A). A skip is not a red; `uid==0` is
   asserted, and a `control_write` via `std::fs` proves the OS permits the
   operation before the permission layer is accused.
