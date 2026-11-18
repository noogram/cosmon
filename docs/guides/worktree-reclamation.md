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

## `[[worktree_reclaim.external]]` — build directories outside the worktree

Some projects keep each worktree's `target` on another volume: `target` is a
symbolic link to a per-molecule directory such as
`/Volumes/Builds-A/cosmon-target/<molecule-id>-wt`. Without a declaration,
that link is refused like any link that leaves the worktree, because its
target could be any directory. A project that names its bases in advance gets
those directories reclaimed ([ADR-191](../adr/191-a-project-may-declare-an-external-derived-root.md)):

```toml
[worktree_reclaim]
evict = ["target"]

[[worktree_reclaim.external]]
root = "target"                          # must also appear in `evict`
base = "/Volumes/Builds-A/cosmon-target"
suffix = "-wt"                           # optional; default ""

[[worktree_reclaim.external]]
root = "target"
base = "/Volumes/Builds-B/cosmon-target"
suffix = "-wt"
```

With these two entries, molecule `m` may own `<base>/<m>-wt` under either
base, and nothing else under either base.

**What is reclaimed.** The payload of `<base>/<m><suffix>` — every entry except
the build locks and their parent directories — when all of these hold:

* the directory is reached through `<worktree>/target` and is exactly the
  worktree's own molecule's child of a declared base, or it is a direct child
  of a base that no live worktree link reaches (an **orphan**, typically left
  by `cs done`, which removes the link and not its target);
* molecule `m` has a record **in this fleet's store** and is `Completed` or
  `Collapsed`. A directory whose molecule this store does not know is kept:
  a base may be shared with another project;
* every `<profile>/.cargo-lock` and `<triple>/<profile>/.cargo-lock` present
  under the directory is acquired, and `debug/.cargo-lock` exists. That is the
  only "in use" test. A process whose arguments name the path (a `du`) does not
  block reclamation, and modification times are not read;
* no other link or base child reaches the same directory, compared by path and
  by device and inode;
* the worktree enumeration and the base's listing had no error.

The same gesture as above applies: dry by default, removal only with
`--allow-unharvested` and without `--dry-run`. Execution re-resolves each link
under the held locks and refuses when it now points elsewhere.

**What is never deleted.** The link itself, which belongs to the worktree; the
lock files and their parents; every base directory and everything under a base
other than the selected directory; anything reached through a link inside the
payload (the link is unlinked, its target is left alone); the worktree.

**What the register says.** Each declared base gets a line — `scanned`,
`unreachable, not scanned` (a volume that is not mounted: the rest of the pass
runs), or `listing failed` — and each external directory is listed with its
resolved path, how it was reached, its base and its size:

```text
  external derived roots (ADR-191):
    base /Volumes/Builds-A/cosmon-target: scanned
    base /Volumes/Builds-B/cosmon-target: unreachable, not scanned — …
    1 external director(ies) withheld, 48.2 GiB:
      - /Volumes/Builds-A/cosmon-target/task-…-0c2d-wt [orphan, base /Volumes/Builds-A/cosmon-target] (48.2 GiB)
          molecule task-…-0c2d is running — a molecule that is not Completed or Collapsed may still be resumed
    1 external director(ies) with reclaimable payload, 51.0 GiB:
      - /Volumes/Builds-A/cosmon-target/task-…-77aa-wt [linked from …/.worktrees/task-…-77aa/target, base /Volumes/Builds-A/cosmon-target] (51.0 GiB)
```

**Declaration errors stop the pass**, naming the entry and the condition: a
relative base; a base that is `/`, the home directory, the repository root, an
ancestor of it, or inside the repository or a worktree; a suffix containing
`/`, `..` or NUL, or longer than 64 bytes; two entries with the same canonical
base and suffix; a base nested inside another; a `root` missing from `evict`.
A name that reads as two molecules under two suffixes of one base (`""` and
`t` make `task-20261118-b7d7t` both `task-20261118-b7d7t` and
`task-20261118-b7d7` plus `t`) is not an error; that child is withheld as
ambiguous.

Not covered: simulator devices, temporary directories outside a declared base,
and deeper layouts such as `<base>/<m>/target`.

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

## `[admission]` — refusing a spawn on machine state

```toml
# Optional. Both bounds unset (the default) means `cs tackle` behaves as before.
[admission]
max_active = 2                  # refuse when this many molecules are active
min_free_bytes = 10737418240    # refuse below 10 GiB free on the volume holding .worktrees/
```

`cs tackle` checks these before the dispatch claim, so a refusal leaves no
worktree, branch or session. It exits 1 and names the bound and the observed
value. There is no bypass flag; wait, or raise the bound.

**Counting rule for `max_active`:** a molecule is active when its status is
`running` and its recorded worker session is alive. `completed` (harvested or
not), `frozen`, `starved`, `collapsed` and awaiting-human molecules are not
counted, and the molecule being tackled never counts against itself. The rule
is one function, `cosmon_core::spawn_admission::counts_as_active`.

**Free space** comes from `df -Pk` on the nearest existing ancestor of
`.worktrees/`. If the probe fails, the spawn is admitted and a notice goes to
stderr. Load average and memory are not inputs (ADR-190 D2). TOML integers are
signed 64-bit, so the largest `min_free_bytes` a file can state is
9223372036854775807.
