# Pair an implementer with a cross-vendor reviewer

**Goal:** run two molecules on one piece of work, an implementer and a
reviewer on a different provider, let them exchange evidence through
`cs work` (ADR-182), and settle both when the implementer finishes or stops.

Messages carry evidence and requests only. They never advance a molecule,
satisfy a dependency or give one seat authority over another. Each molecule
keeps its own lifecycle, branch and harvest.

## 1. When to pair

Pair when an independent recomputation or an adversarial check is worth a
second seat. In the field, the most useful line in a review brief was
"recompute one number independently from the saved outputs".

## 2. Launch

Nucleate the implementer, then the reviewer with the implementer's id as a
variable, so no brief needs a `sed`. The owner of the work is the implementer.

```sh
impl=$(cs nucleate task-work --var topic="<the task>" --json | jq -r .id)
rev=$(cs nucleate task-work --var topic="review $impl" --var impl="$impl" --json | jq -r .id)
cs work declare "$impl" --seat impl="$impl" --seat review="$rev"
cs tackle "$impl" --adapter claude --model <model>
cs tackle "$rev"  --adapter codex  --model <model>
echo "impl=$impl review=$rev"
```

Pin the model explicitly on each seat. Do not rely on an account default.

## 3. Implementer brief

Include: the goal, the operator request verbatim, inputs by path, numbered
items, constraints and deliverables. Then:

- Read `cs work inbox` at each milestone and ack every message with a
  disposition.
- Send `cs work send --to review --phase final` naming the final commit before
  `cs complete`.
- Run the full gate once, at the end.
- If the gate fails outside your diff with the work committed, collapse with
  `--reason-kind verification_blocked --reason 'work committed at <sha>; <gate> failed: <class>'`.
- Write notes to a per-molecule path (see section 9).

## 4. Reviewer brief

- Find the peer with `cs work list` (no owner: your own roster), or use the
  implementer id passed as a variable.
- Challenge at each milestone with `cs work send --to impl`.
- Give 3 to 6 task-specific checks.
- Recompute one number independently from saved outputs.
- Every verdict names the implementer commit it reviewed.
- Do not `cs complete` until the implementer has sent `final` or is terminal;
  the last act is `cs work inbox`.
- Write and commit the review file.

## 5. Steering

Scope, rules, hold, stop and unblock go through `cs whisper`, one per seat.
A roster message can be acked `--rejected`; a steering order must not be. If
you would not accept `--rejected` as an answer, it is not a roster message.
A real stop is `cs collapse`.

## 6. Watching

`cs work list <owner>` shows each message's stage. `cs observe <seat>` shows a
seat's molecule. Inside a member, an omitted owner means that member's own
roster; it is never a fleet-wide view.

- Implementer collapsed while the reviewer is active: decide how to settle
  (section 7).
- Reviewer completed while the implementer is active: compare the commit the
  verdict names with the implementer's head.

## 7. When the implementer terminates

Read the collapse reason kind first.

- `verification_blocked` is a documented convention, not a typed outcome. It
  makes infrastructure-blocked collapses findable with
  `cs errors --kind verification_blocked`. It is the worker's claim and never
  evidence that the branch is safe to merge: check `git log main..<branch>`
  and whether the failing test touches the diff, then decide yourself.
- For any other kind, audit the branch before deleting it.

To settle a pair in one command:

```sh
cs collapse <impl-or-review> --with-seats --reason "<why>" [--reason-kind <kind>]
```

This resolves the declared work of the named molecule, collapses every live
seat with the same reason through the ordinary `cs collapse` path, skips a
completed seat and prints `cs done <seat>` for it, and merges nothing. When
the named molecule is already collapsed, `--reason` may be omitted: its
recorded reason is inherited as `"<reason> (seat <name> of work <owner>@<revision>)"`.
The exit code is non-zero if any seat failed, and a rerun converges from each
molecule's own state. Then harvest with `cs done` per seat; add `--no-merge`
for an implementer you already merged by hand.

## 8. Continuation

1. Settle the old pair (section 7).
2. Launch a new pair under a new owner, with `cs nucleate --decayed-from <old>`
   on both molecules.
3. Open the brief with "Continuation of `<old id>` (`<reason>`): merged, do not
   redesign: …; remains: …", then the original brief for reference.
4. The new reviewer reads the merged review file, not old envelopes.

Never re-declare a seat name onto another molecule under the old owner.
Delivery refuses to hand an earlier holder's pending envelopes to a new one,
but a new owner is the clear form. The cost is that the new reviewer starts
cold; that is accepted.

## 9. Shared notes

Write one file per molecule, for example `notes/<mol_id>.md`, and regenerate
any aggregate on the main branch. Never union-merge code.

## 10. Cost

Put the reviewer on the other or the cheaper provider. Use high-reasoning
models for framing and arbitration only. No spend figures are measured yet, so
none are quoted here.
