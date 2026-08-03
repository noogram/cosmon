<!--
Thanks for the change. Keep the summary tight; put the reasoning
in the description or link to an ADR / molecule / chronicle.
-->

## Summary

<!-- One or two sentences on what this PR does and why. -->

## Linked artifacts

<!-- Molecule IDs, ADRs, chronicle entries, or deliberation refs. -->

## Coherence checklist

Start with the [ten-check summary](https://github.com/noogram/cosmon/blob/main/docs/architectural-invariants.md#start-here--ten-checks-for-a-typical-pr),
then use §5 for details. Check each applicable line. For every unchecked line,
write `N/A — <reason>` so a reviewer can verify why it does not apply.

- [ ] I ran the changed command once and verified that the command itself exited without leaving a control loop running in Layer A.
- [ ] I ran the same operation twice against disposable state and verified that the second run was a no-op or produced the same state; otherwise I tested the explicit retry guard.
- [ ] I named the affected regime(s)—Inert, Propelled, or Autonomous—and tested or explained behavior at each boundary the change crosses.
- [ ] I compared the behavior with the command-perimeter table in §3 and verified that no existing command already owns it.
- [ ] I named the teardown counterpart for every file, session, branch, registration, or state this change creates and tested the create/undo round trip.
- [ ] I verified that the behavior still works when the resident runtime invokes the transactional core, or documented and tested the intentional regime restriction.
- [ ] I identified the command as worker-callable or human-only and verified that a worker-callable path cannot destroy its own worktree or session.
- [ ] I verified that no invocation both mutates state and returns a post-write coupling report; the report comes from a separate read invocation.
- [ ] I tested that dependent work cannot dispatch before its predecessor branch is merged.
- [ ] I ran every worker-callable path through `cs` from inside a nested worktree and verified that it uses walk-up discovery without MCP.
- [ ] I tested traversal with an unrelated or completed molecule present and verified that the out-of-scope molecule was unchanged.
- [ ] I exercised the capability at the adjacent applicable level—molecule, polymer, or fleet—or explained why no adjacent level exists.
- [ ] I checked the diff for persisted molecule fields, state mutations, and read-couplings; each is represented in `docs/specs/CosmonRun.tla` in this commit or documented as out-of-band in `docs/lore/logicien-register.md`.

## Test plan

<!-- Include `just gates` and any focused or behavior-specific checks. -->
