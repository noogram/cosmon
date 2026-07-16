<!--
Thanks for the change. Keep the summary tight; put the reasoning
in the description or link to an ADR / molecule / chronicle.
-->

## Summary

<!-- One or two sentences on what this PR does and why. -->

## Linked artifacts

<!-- Molecule IDs, ADRs, chronicle entries, or deliberation refs. -->

## Coherence checklist

See `docs/architectural-invariants.md` §5. Check every box that
applies, or justify the skip in the description.

- [ ] **Stateless / Idempotent / Regime-aware** — no daemon in Layer A,
  twice = once, the right regime(s) noted.
- [ ] **Single perimeter** — no overlap with an existing command's role.
- [ ] **Symmetric undo** — creation has a matching teardown.
- [ ] **Runtime-compatible** — still makes sense when the resident
  runtime owns L3.
- [ ] **Worker/human boundary respected** — worker-callable code does
  not self-destroy; human-only commands assume the worker is done.
- [ ] **Write-read asymmetry preserved** — no command both writes state
  and returns a coupling report in the same invocation.
- [ ] **Merge-before-dispatch respected** — predecessor's branch merged
  before dependent is dispatched.
- [ ] **CLI-first for workers** — walk-up discovery, not MCP.
- [ ] **Scope-bounded / Self-similar** — traversal stays inside the
  intended subgraph; the capability composes at adjacent levels.
- [ ] **AC (Alphabet Closure).** If this PR adds a persisted field,
  mutating action, or read-coupling on molecule state, the spec edit
  landed in the same commit, OR the field is documented as out-of-band
  in `docs/lore/logicien-register.md`.

## Test plan

<!-- Gates you ran (cargo check/test/clippy/fmt) + anything extra. -->
