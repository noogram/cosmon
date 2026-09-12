<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Reclaiming disk from `.worktrees/`

`cs purge --worktrees` reports what a galaxy's worktree directory is holding
and, when asked, reclaims the rebuildable part of it. This guide is about the
one question everything else hangs off: **what the build lock does and does not
cover.**

Related: [ADR-178](../adr/178-no-automatic-path-removes-a-worktree.md) (no
automatic path removes a worktree) and the working contract at
[`docs/design/worktree-reclaim/CONTRACT.md`](../design/worktree-reclaim/CONTRACT.md).

## What the command does

```text
cs purge --worktrees                       # report; remove nothing
cs purge --worktrees --allow-unharvested   # report; reclaim derived output
cs purge --worktrees --dry-run             # report, even with the gesture above
cs tackle <molecule> --reclaim-derived     # the same pass, before a spawn
```

It enumerates `readdir(.worktrees/) ∪ git worktree list --porcelain`. That
union is the point of the feature. Every earlier mechanism was keyed by
*molecule* or by *worker*, so a directory with neither could not be reached at
all: on the repository where this was reported, fourteen directories existed,
five had no molecule, and the worker roster could see two. The molecule is
still consulted — as a **veto**, never as the way a candidate is found.

Then it reports two lists. The withheld list comes first, because after a run
the operator's question is "what is still holding disk?", and each entry names
its own reason:

```text
  2 worktree(s) withheld, 4.1 GiB:
    - .worktrees/task-20260101-dead [on disk, unregistered]
        durable: git does not know this directory as a worktree — unregistered
                 scratch, whose contents nothing else holds a copy of
    - .worktrees/task-20260910-7c11 [on disk and registered]
        derived: another process holds the build lock at …/target/debug/.cargo-lock
```

## The lock: what it covers

Exclusion comes from one thing: a non-blocking exclusive `flock` on
`<worktree>/target/debug/.cargo-lock`, Cargo's own build lock, held for the
whole reclamation.

**It covers Cargo.** Any `cargo build`, `cargo test` or `rust-analyzer` writing
into that `target/` takes the same lock first, so acquiring it means no Cargo
process is producing into the directory whose payload is about to go.

**It covers nothing else.** A lock excludes exactly the producers that take it.
`xcodebuild`, `cmake`, `webpack`, a `Makefile`, a hand-run script — none of
them have ever heard of `.cargo-lock`, and holding it says nothing whatsoever
about what they are doing.

Three consequences follow, and all three are visible in the output:

* **A missing anchor is not permission.** If `target/debug/.cargo-lock` is not
  there, the probe reports a failure, not an absence of contention. Nothing is
  reclaimed. The probe never creates the file it failed to find — manufacturing
  the anchor would manufacture the exclusion it exists to detect.
* **The anchor survives reclamation.** Selecting `target/` takes its *payload*;
  the lock file and its parent directories stay. Holding a file descriptor
  while unlinking its name does not stop another process from creating a new
  inode at the same path, so the pathname's identity is preserved for the
  protected interval.
* **A non-Cargo root is withheld.** See below.

## `[worktree_reclaim].evict`

```toml
[worktree_reclaim]
evict = ["target", "build/ios"]
```

The default is `["target"]`. The key is configurable because `target/` is a
Cargo fact and not a universal one: a galaxy that is Rust *and* an iOS
staticlib carries rebuildable bytes outside `target/`, and a hardcoded list
would either miss them or invite a galaxy-specific fork of this code path.

Naming a root does not make it reclaimable. Each root is first validated as a
rebuildable, worktree-local relative path — no absolute path, no `..`, no Git
metadata, not the worktree root itself, and no symlink that resolves outside
the candidate. Then it must have an **establishable exclusion**, and today
that means containing the Cargo anchor. `build/ios` does not, so it is
enumerated, reported, and left alone:

```text
    - .worktrees/task-20260910-7c11 [on disk and registered]
        root …/build/ios: no exclusion protocol covers `build/ios`: the
        acquired lock is Cargo's own build lock at `target/debug/.cargo-lock`,
        which excludes Cargo and no other producer
```

This is deliberately visible rather than silent. A configured root that
quietly did nothing would be the same invisible leak as the directories the
roster could not see, only pointing the other way.

To make such a root reclaimable, an exclusion protocol for its producer has to
exist — a lock the producer actually takes. Until then the honest answer is to
report the bytes and keep them.

## What is never reclaimed

No path in cosmon removes a worktree automatically (ADR-178). The durable
eligibility verdict — registered, provably zero commits ahead of base, clean
status, no ignored-but-durable content — is computed, printed, and acted on by
nobody. Every conjunct in it is an observation, and an observation is a claim
about the past: between a clean `git status` and a `remove_dir_all`, an agent
can write a file, and on a cosmon fleet agents write files in worktrees
continuously and by design. Derived reclamation survives that race because it
holds the producer's lock across the interval. There is no equivalent lock over
an editor, a detached worker pane, or a half-finished rebase.

Removing a worktree stays a thing a human asks for by name: `cs done` after a
harvest, or `git worktree remove`. The register above is what makes that choice
informed.
