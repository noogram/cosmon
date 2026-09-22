<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ADR-179 — A terminal molecule's tmux session is derived state

**Status:** Accepted (2026-09-22).
**Date:** 2026-09-22.
**Decider:** Noogram.
**Authoring molecule:** `task-20260920-e641`.

**Scope.** This ADR decides that a tmux session whose molecule is terminal may
be reclaimed automatically, and states the exclusion argument that permits it.
It removes no worktree and widens no worktree path.

**Binds:**
[ADR-178](178-no-automatic-path-removes-a-worktree.md) (whose doctrine this
carries onto a different resource, and whose D1/D4 it leaves intact),
[ADR-082](082-architecture-baseline.md) (the I/O-free domain core behind
injectable ports),
[ADR-052](052-one-ledger-one-writer-one-witness.md) §D3 (`cs purge` is the one
infrastructure-teardown verb).

---

## 1 · Context

`cs purge`'s sweep has always had a third population beside "stopped" and
"stale": a worker whose tmux session is alive while its `current_molecule` is
`Completed` or `Collapsed`. The sweep removed the fleet entry and, by an
explicit choice recorded in a code comment, left the session alone — "the agent
may still be sitting at `❯`; the operator decides whether to kill the session".

That choice is what makes the leak permanent rather than merely untidy. The
roster entry was the **only** link from a session back to its molecule. The
moment it is removed the session becomes unattributable: no later invocation of
anything can say which molecule it belonged to, so nothing ever looks at it
again. The population is therefore a ratchet — it only grows.

Measured on the development machine on 2026-09-22, across five galaxy sockets:
fifteen sessions, holding **4.0 GB** resident between their pane processes, in
an aggregate of 23 GB across 260 agent processes. Not one of the fifteen was an
idle shell. That is the fact that reframes the problem: an agent that finishes
its molecule does not exit. It calls `cs complete`, returns to its prompt, and
sits there holding its entire heap. The bytes are not tmux's; they belong to a
`claude` or `codex` process whose reason to exist ended hours earlier.

## 2 · Decision

**D1.** A tmux session whose owning molecule is terminal, with no client
attached and its scrollback already captured, is derived state and may be
reclaimed by an automatic path. `cs purge --sessions` is that path.

**D2.** Ownership must be **positive**, and it is established by computing each
known molecule's session name forward (`slugify::session_name_for`) and
matching. A session matching no molecule is `Unowned` and is **withheld**. A
molecule id is never parsed back out of a session name: the name carries four
characters of it, and four characters are a guess.

**D3.** The scrollback is the session's durable content, and it is carried out
of the way before anything is killed — captured into the owning molecule's
directory. A session whose scrollback could not be secured is withheld. There
is no flag that reclaims a session whose scrollback failed to capture.

**D4.** The opt-in is the one `--worktrees` already uses: `--sessions` reports,
`--allow-unharvested` executes, `--dry-run` overrides. Not a second gesture.

**D5.** ADR-178 D1 and D4 are untouched. No worktree is removed by this path.

## 3 · Why this may be automatic where a worktree may not

ADR-178 refuses automatic worktree removal on one argument, and it is a good
one: *every conjunct is an observation, and an observation is a claim about the
past.* `git status` was clean when it ran; between that moment and the
`remove_dir_all` an agent can write a file, and cosmon holds no lock over an
operator's editor. Eligibility without exclusion is a snapshot, not a
guarantee.

Neither half of that argument survives the move to a session.

**The gate is monotone.** `MoleculeStatus::can_transition_to` admits no
transition *out of* `Completed` or `Collapsed` — neither appears on any
left-hand side. A molecule observed terminal is terminal for good, so there is
no interval during which the observation can decay. This is exactly the
property the worktree conjuncts lacked, and it is asserted by a test against
the transition table rather than restated in prose, so that adding such a
transition later fails a build instead of silently invalidating this ADR.

**The durable content is bounded and capturable.** A worktree's durable bytes
are unbounded and git-owned; there is nowhere to put them. A pane's scrollback
is one `capture-pane` away from a file. So the durable half is not traded
against the reclamation — it is moved first, and a failed move withholds. What
remains is the pane, the process and the heap, all of which `cs tackle`
rebuilds by definition.

The asymmetry of the two mistakes — the actual content of ADR-178 — is
therefore preserved rather than overridden. A session wrongly reclaimed costs a
respawn and a scrollback file written a moment earlier. That is the cheap
mistake. Every axis is arranged so the expensive one needs positive evidence.

## 4 · Why absence inverts

For a worktree, a proven-absent molecule **permits** consideration:
`.worktrees/<id>/` is cosmon's own directory, so a directory nobody claims is
still unambiguously cosmon's to reclaim. Location is ownership evidence.

A tmux socket is a shared namespace. An operator may run their own session on
it, and a session name is not evidence of anything. So the same input —
"nothing claims this" — must produce the **opposite** verdict, and D2 says it
does. This is the one place a reader who knows ADR-178 would expect the other
answer, which is why it has a test of its own asserting the two gates disagree
on purpose.

## 5 · What this ADR does not say

It does not say a live agent process protects its session. It does not: the
process *is* the leak, four gigabytes of it, and whether an agent is still
running says nothing about whether its molecule is finished. Pane liveness is
absent from the predicate's inputs by construction, so it cannot later be
taught to spare a process the gate has already condemned.

It does not introduce a byte threshold, an age-based escape, or a background
timer. As in ADR-178, a destructive action that runs unobserved is outside the
perimeter of every decision above: this one runs when an operator types
`cs purge --sessions --allow-unharvested`.

It does not claim the scrollback is a complete record. It is what the pane
still held — `capture-pane -S -` reaches the top of the scrollback buffer, not
the beginning of time. It is strictly more than the nothing that was kept
before.

## 6 · Consequences

* The ratchet is broken in both directions: sessions orphaned by past sweeps
  are attributable again through forward name computation, and newly terminal
  ones are reclaimed in the same pass that removes their fleet entry.
* `cs purge --sessions` without `--allow-unharvested` is a register an operator
  can read: every session on the socket, reclaimable or withheld, each withheld
  one with the reason that withheld it.
* A galaxy wanting to reclaim sessions on some other ground — age, memory
  pressure, a live-agent heuristic — must write ADR-N and state its own
  exclusion argument. Monotonicity is the bar, and it is the bar because it is
  the thing that actually makes the reclamation safe.
