# ADR-166 — The root → uid demote path is refused, not narrowed again

**Status:** Accepted (2026-07-28).
**Date:** 2026-07-28.
**Decider:** Noogram.
**Authoring task:** `task-20260728-469b`.

**Entry artefact.** `converge-20260727-a302` exited `BLOCKED` at
`max_rounds` with one HIGH still open, reproduced twice by the loop and once
independently by a committee seat: the demote hand-over grants `objects` +
`refs/heads` + `logs/refs/heads` on the repository's shared common dir, which
is enough for a demoted worker to rewrite a sibling molecule's branch and to
delete an object another molecule's history depends on. All six gates were
green at `0d6c264`. Green gates were not the verdict; this is precisely what
they do not measure.

**Supersedes:**
[ADR-165 §2](165-resources-are-created-under-the-identity-that-consumes-them.md)
("Compatibility — root → uid stays, and stays fail-closed"). ADR-165's
*nominal* decision — one identity, no hand-over — is unchanged and is what
this ADR leans on.

**Related ADRs:**
[ADR-162](162-dispatch-boundary-ready-is-earned.md),
[ADR-163](163-a-question-may-only-be-asked-where-an-answer-can-arrive.md).

---

## Context

### The one defect that did not close by adjusting the hand-over

The grant has been cut three times in two days: the worktree, the config-home
consent files, the gitdir; then cut too wide (the whole common dir, including
`hooks/`, which the dispatcher executes as root at `cs done`), then re-narrowed
at the merge. Each cut was correct. Each left a residue.

The last residue does not yield to a fourth cut, for a structural reason that
belongs to git and not to cosmon:

- **`refs/heads` moves as a directory.** A loose-ref store creates
  `<branch>.lock` beside `<branch>`, and a cosmon branch is `feat/task-…`, so
  the directory holding this worker's branch also holds every sibling
  molecule's. Git's files backend offers no per-ref delegation; a sibling ref
  moved into `packed-refs` is shadowed by a loose ref the worker may create;
  and `git update-ref` is plumbing that ignores the "checked out in another
  worktree" guard.
- **`objects` moves as a whole store.** Writing a loose object means creating
  the `objects/XX/` fan-out directory, which needs write on `objects` itself,
  and owning `objects/XX/` is owning every object in it.

So the grant that is small enough to be safe is not large enough to commit,
and the grant that is large enough to commit is repository-wide destructive
authority over every molecule in flight. That trade-off was proven rather than
assumed: `sibling_rewrite` and `shared_object_delete` both SUCCEEDED at uid
10001 in a Linux container, and again at uid 501 on macOS through a mode-bit
freeze.

### This is not the `cs done` question

ADR-165 opened a separate boundary — with a shared uid, nothing in POSIX stops
a worker from *invoking* `cs done` — and named a typed cosmon authorisation as
its answer. That is a control-plane question and it is open as its own
molecule.

It is not this one, and conflating them is an error this lineage has already
made once. With `objects` + `refs/heads`, a worker rewrites a sibling branch
or deletes a shared object **without calling `cs done` at all**. A future
typed `cs done` capability does not close this. Neither may be argued as a
mitigation for the other.

### Refusing costs nothing now

The nominal path is externally validated: the external tester replicated the
non-root pilot on his own bench — two consecutive dispatches reaching terminal
state, `cs done` merging and cleaning up, the first end-to-end close there —
with no `safe.directory` exemption for the worker uid
([bench](../benches/nonroot-pilot-2026-07-28.md)). Refusing the dangerous path
therefore blocks nobody, which is what makes fail-closed the cheap choice here
rather than the expensive one.

## Decision

**A root dispatcher with a demote target declines to spawn.**

`decide_root_spawn(0, Some(uid))` returns
`RootSpawnDecision::Refuse { reason: DemoteSharesRepositoryStorage { uid } }`.
For `running_uid == 0` the result is now *always* a refusal, which is strictly
stronger than the original contract-20A guarantee (never a live root worker)
and subsumes it.

Three properties make the refusal a control rather than a caveat:

1. **It is typed.** `root-spawn-refused:demote-shares-repository-storage`, a
   stable machine token, recorded as a root-refusal event exactly like the
   other two reasons.
2. **It is reachable.** The `Display` names the uid to run as, spells the
   `docker exec -u <uid>:<uid>` invocation, and points at the container guide.
   §8z: a caveat the operator cannot read is not a control, and neither is a
   remedy.
3. **It precedes every write.** The decision is I/O-free and is taken at the
   entry of `cs tackle`, before the config home is created, before the fleet
   state is written, before `.worktrees/` exists and before git is invoked on
   the repository — as well as before the startup-consent pre-grant, before any
   `chown`, before the cognitive probe and before any process exists. A refused
   dispatch **adds no path and changes no ownership or mode** under the galaxy
   root or the Claude config home. The one content mutation it makes is the
   append-only fleet refusal record, which is stated below rather than elided —
   "byte-identical" would be the stronger claim, and it would be false.

   **This clause was false as written when this ADR was published, and the test
   it cited did not carry it.** The refusal lived inside
   `spawn_claude_and_prompt`, so it preceded the worker and the probe but not
   the provisioning: one `sudo cs tackle` on the galaxy the container guide
   describes exited 1, spawned nothing, and left root-owned `.claude.json`,
   `settings.json`, `.worktrees/`, `.git/config`, `.git/packed-refs`,
   `fleet.json` and `fleet.runtime.json` — after which the *documented* non-root
   dispatch died with `mkdir: Permission denied` on `.worktrees/`. The cited
   test asserted the worktree's owner was unchanged, which is true and
   uninformative: a refused dispatch never creates a worktree. What it created
   was the `.worktrees` **parent**, which that assertion never looked at.

   What now carries the clause is
   `a_refused_root_dispatch_adds_no_path_and_changes_no_ownership_or_mode`
   (`crates/cosmon-cli/tests/refused_root_dispatch_leaves_no_residue.rs`),
   which names no path: it snapshots every entry under the galaxy root and
   under the config home with owner, group and mode, runs the refused dispatch
   end to end through the real `cs` binary, and asserts the two sets are
   identical. A residue nobody predicted fails it. The scope of the older
   assertion (`a_root_dispatch_refuses_without_touching_the_filesystem`) is
   narrowed in its own doc comment to what it actually measures: the
   *provisioning funnel* performs no `chown` on the refuse arm.

   One thing is enforced and worth stating precisely, because it is the edge of
   the claim: the typed refusal is *recorded* to `events.jsonl`, which is a
   write. It is not residue because the sinks are opened **append-only** — a
   refused dispatch never creates an events file it found missing.

   Two earlier drafts of this clause described a "per-molecule sink" as though
   a molecule had an event journal of its own. **It does not, and it never
   did.** What the append-only rule changed is narrower than either draft said,
   and the difference is worth stating because it was found by external review
   twice in a row.

   There is one event journal: the galaxy ledger at
   `.cosmon/state/events.jsonl`. It exists from `cs init` onward, the refusal is
   recorded there, and it is what the container repro harness and the pinning
   test read.

   A molecule directory may *also* contain an `events.jsonl`, and that file is
   not a lifecycle journal — it is a side-file that diagnostic probes create
   when they happen to run. Measured across the development galaxy: 164 of 389
   molecule directories carry one, and of those 164, **152 contain nothing but
   `adapter_pane_signature_checked`**. The only four event types that appear
   anywhere in them are that probe, `adapter_liveness_probed`, `model_observed`
   and `worker_spawn_attempted`. A molecule that ran to `completed` without a
   pane probe has no such file; a molecule refused before dispatch has none
   either.

   Before this change the refusal record opened its sinks with `create`, so a
   refused *root* dispatch produced that file as a side effect and the token
   appeared in it. That was the file being manufactured by the very write the
   refusal exists to prevent. Append-only removes the side effect, which is
   correct, and it does not remove a journal — there was none to remove.

   The consequence stated honestly, and it is broader than the refusal case:
   molecule-scoped tooling cannot explain why a molecule never started, and it
   could not explain a molecule that ran fine either. The fleet ledger is where
   every answer lives. Whether a real per-molecule journal should exist is a
   separate question this ADR does not decide.

`RootSpawnDecision::Demote` and the transport-side provisioning port are kept,
dormant and unreachable from the policy, for two reasons: they are the
substrate the bounded lifecycle below will re-enable, and the tests that
characterise the grant have to be able to *build* it. A test that can only
reach the machinery through a policy that refuses it measures the refusal
instead — which is how three green suites sat on top of an open hole in this
very lineage. The dormant arm is therefore entered explicitly by those tests,
through `provision_demote_resources`, which returns resource verdicts and
never a decision, so no caller can spawn on it.

## What is deferred, named rather than half-built

Per-worker ref **and** object storage: a per-worker repository reaching the
shared store read-only through `objects/info/alternates`, with `cs done`
**fetching** rather than merging in place. That is a different worktree
lifecycle, not a tighter `chown`. It is not attempted here. When it lands,
`the_grant_still_permits_a_sibling_ref_rewrite_and_a_shared_object_delete`
goes red, and that red is the signal the refusal may be lifted.

## Consequences

- Piloting a container as root with `COSMON_WORKER_UID` set now fails at
  `cs tackle` with a refusal naming the non-root invocation. The container
  guide says so, with the message quoted.
- ADR-165 §2 is superseded. §8y (what cosmon repairs and what cosmon judges
  are one list) still governs the dormant repair set; it now governs a path
  nothing takes.
- The residue tripwire keeps its subject but changes its meaning: it is no
  longer a description of live behaviour but the evidence the refusal rests
  on. It also stopped being flaky — it used to pick its victim object with
  `read_dir`, whose order is unspecified, so in roughly three runs in ten it
  deleted the *worker's own* commit and proved nothing. It now derives the
  sibling's tip object by name.
- What the fleet loses: nothing measured. What it gives up in principle is the
  ability to pilot from a root container, which no validated recipe uses.
