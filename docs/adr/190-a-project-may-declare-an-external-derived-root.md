<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ADR-190 — A project may declare an external derived root; `cs purge --worktrees` reclaims it for terminal molecules

**Status:** Proposed (2026-11-16).
**Date:** 2026-11-16.
**Decider:** Noogram.
**Authoring molecule:** `task-20261116-ac57`.
**Answers:** GitHub issue #218 (second half), follow-up to issue #61.

**Scope.** This ADR decides whether, and under which declaration, the
derived-reclaim pass behind `cs purge --worktrees` may remove a build
directory that lives *outside* the worktree and is reached through a
symbolic link. It decides the rule and its refutation tests. It changes no
code; the implementing molecule owns the config spelling, the adapter and
the CLI presentation.

**Binds:**
[ADR-178](178-no-automatic-path-removes-a-worktree.md) (no automatic path
removes a worktree; durable eligibility is advisory),
[ADR-052](052-one-ledger-one-writer-one-witness.md) §D3 (`cs purge` is the
one infrastructure-teardown verb; no new verb),
[ADR-082](082-architecture-baseline.md) (I/O-free core behind injectable
ports),
the working contract at
[`docs/design/worktree-reclaim/CONTRACT.md`](../design/worktree-reclaim/CONTRACT.md)
and its witness `tests/harness/worktree-reclaim-contract.py`.

---

## 1 · Context

### 1.1 What the reporter has

The reporter of #218 runs four to nine molecules at once. In each worktree,
`target` is a symbolic link to a per-molecule directory on another volume.
Each such directory holds 45–54 GB; a 1 TB volume fell to 60 GB free within
hours. They ask for a scoped `cs gc` that removes per-molecule artefacts of
terminal molecules — the worktree, registered extra paths such as build
directories, simulator devices, per-molecule temporary directories — limited
to the current project's fleet and skipping anything touched recently.

They run their own cleanup loop today and report two traps:

* **The `du` trap.** A process whose command-line arguments merely contain
  the path (for example `du -sh <path>`) was counted as "in use" and blocked
  deletion.
* **The `readlink` trap.** Some workers ended up with a real build directory
  inside the worktree instead of the link, so a cleaner must check what is
  actually there and not assume the layout.

### 1.2 What cosmon does today (read in the code)

`cs purge --worktrees` runs one pass
(`crates/cosmon-cli/src/cmd/worktree_reclaim.rs`, `run_pass`): enumerate
`readdir(.worktrees/) ∪ git worktree list --porcelain`, observe each
candidate on the contract's axes, decide with the two pure predicates in
`crates/cosmon-core/src/worktree_reclaim.rs`, report every withheld
candidate with a reason, and — only with `--allow-unharvested` and without
`--dry-run` — remove the derived selection. The same pass runs with
`execute = true` from `cs tackle --reclaim-derived`, the opt-in pre-spawn
pressure check (`crates/cosmon-cli/src/cmd/tackle.rs`, `run_pressure_check`).

For the reporter's layout the pass reclaims nothing, by design:

* The adapter's `derived_roots` (`crates/cosmon-harvest/src/worktree_reclaim.rs`)
  canonicalizes each configured root and requires the result to start with
  the canonicalized candidate. A `target` link to another volume fails that
  check and the adapter returns `DerivedRoots::Unavailable` with operation
  `"contain derived root"` — for the **whole candidate**, by early return,
  not only for that root. Test `symlinked_derived_root_is_unavailable` pins
  this and asserts the link target survives.
* A **dangling** link fails `path.exists()` and is skipped as "known absent",
  contributing the empty set without a reason.
* A **real** `target/` directory inside the worktree passes the containment
  check and is reclaimed under the existing rule. The reporter's `readlink`
  trap is therefore already handled correctly for the real-directory case.
* The lock probe opens `candidate.join("target/debug/.cargo-lock")` with
  `File::options().read(true).write(true).open`, which follows symbolic
  links. With a link in place, the flock is taken on the anchor **inside the
  external directory** — the same file Cargo locks when it builds there.
* `approximate_size_bytes`, used for the withheld register, follows no
  symlink. For the reporter's layout the register under-reports each
  candidate by the size of its external build directory.
* `cs done` removes the worktree with `git worktree remove`
  (`crates/cosmon-harvest/src/transaction.rs`, `remove_worktree`). The
  `target` link goes with the worktree directory; the external directory it
  pointed to is left behind (checked by hand on a scratch repository:
  `git worktree remove` exits 0, the worktree and its ignored `target` link
  are gone, the link target's contents remain; no cosmon test covers it), and no enumeration in cosmon reaches it again:
  `enumerate_candidates` starts from `.worktrees/` and Git's registry, both
  of which no longer name it. This orphan is the reporter's main leak.

### 1.3 Why the current refusal is right and incomplete

The containment check exists because a derived root that leaves the worktree
through a link can point anywhere: at a shared `CARGO_TARGET_DIR`, at
another molecule's build directory, at a home directory. Deleting "the
contents of whatever `target` points to" is deleting an arbitrary path
chosen by whoever last wrote the link. The refusal is correct for an
*undeclared* target. What is missing is a way for a project to say where
its external build directories live, so that a link resolving there can be
recognised as derived and per-molecule rather than arbitrary.

---

## 2 · Decision

### D1 — An external derived root requires a project declaration

A project may declare, per derived root, one **external base**: an absolute
directory under which per-molecule copies of that root live. Proposed
spelling (final spelling is the implementing molecule's):

```toml
[worktree_reclaim]
evict = ["target"]

[[worktree_reclaim.external]]
root = "target"                       # must also appear in `evict`
base = "/Volumes/Builds/cosmon-target"
```

The base is validated when the pass starts, like `evict` today; an invalid
base is a configuration error and the pass does not run. It must be
absolute; it must exist and canonicalize; its canonical form must not be
`/`, the user's home directory, the repository root, an ancestor of the
repository root, or a path inside the repository or any worktree. A base
that fails any of these is refused with the condition named.

Without a declaration, nothing changes: a link that leaves the worktree is
`Unavailable`, exactly as today, and `symlinked_derived_root_is_unavailable`
keeps passing unchanged.

*Failure prevented:* deleting through an arbitrary link target. Only a
directory the project named in advance can ever hold reclaimable external
bytes.

### D2 — The link target must be the molecule's own child of the base

For a candidate worktree `w` owned by molecule `m`, a link at `w/<root>` is
an external derived root only if its canonical target is **exactly**
`canonical(base)/<m>` — a direct child of the base whose name is the
molecule id. Any other resolution is refused with a reason and selects
nothing:

* resolves outside the base → refused (as today);
* resolves to the base itself → refused (a shared, persistent target);
* resolves to `canonical(base)/<other-id>` → refused (another molecule's
  directory);
* resolves deeper, to `canonical(base)/<m>/sub` → refused (not the layout
  the declaration names).

In addition, a resolved external directory must be **unique across a
complete enumeration**: if two candidates' links resolve to the same
directory, neither is selected. The naming rule already makes this
impossible for well-formed links; the cross-check is the defence for
everything the naming rule assumes. Uniqueness can only be established over
a complete population, so a pass with non-empty `enumeration_errors` selects
**no** external root (in-worktree roots keep their current behaviour).

*Failure prevented:* deleting a directory shared by two molecules, or a
persistent target such as a global `CARGO_TARGET_DIR` placed at the base
itself.

### D3 — What is deleted and what is never deleted

**Deleted:** the payload of `canonical(base)/<m>`, using the same walk as
the in-worktree rule (`reclaim_root`): every entry except the lock anchor
`debug/.cargo-lock` and its ancestor directories, without following
symbolic links inside the payload (a link inside is unlinked; its target is
untouched).

**Never deleted:**

* the link `w/<root>` itself — it belongs to the worktree, and only the
  existing owners of whole-worktree removal (`cs done`, an operator's
  `git worktree remove`) remove worktree entries (ADR-178 D3);
* the lock anchor and its ancestors, for the reason the contract already
  gives: unlinking a locked file's name lets another process create a new
  inode at the same path;
* the base directory and anything under it other than `<m>`;
* anything reached only through a link inside the payload.

Execution re-resolves the link under the held lock and refuses if the
canonical target differs from the one observed (a link swapped between
observation and execution). The dry-run observation is not mutation
authority, as the contract already requires.

### D4 — Eligibility reuses the gate; external roots also require a record

The consideration gate `G` is reused unchanged for candidates whose
external root is reached through a link: `Completed` or `Collapsed`
permits, every other status (including `Frozen` and `Starved`) and an
unknown status veto.

Requiring a molecule to be both terminal *and landed* does **not** become a rule.
Derived selection is ancestry-blind by construction in the contract and in
`DerivedObservation`, and a rebuildable payload's safety does not depend on
whether its source branch merged. `Collapsed` is permitted for the same
reason.

One restriction is added for external roots: the molecule record must be
**Present in this fleet's store**. `G` admits `M = Absent` for
`.worktrees/` directories because those directories are inside this
repository; an external base is not, and may be shared with another
project's fleet. A molecule-less candidate keeps the in-worktree rule and
never reaches an external root.

**No new axis is needed.** Containment, the naming rule and uniqueness are
root validation, which the contract and the code already keep outside the
seven-axis table (`DerivedRoots` is documented as "not an axis of the truth
table"). The 5,832-row witness is unchanged. The "Present for external
roots" restriction is a property of the new candidate class in D6, written
into the contract by the implementing molecule as a separate subsection,
not as a change to the G table.

*Failure prevented:* reclaiming the build directory of a resumable
molecule, or of a molecule that belongs to another project sharing the same
volume.

### D5 — The "in use" test is the build lock, never a process listing

What cosmon can honestly observe, and what it uses:

* **An acquired flock on `canonical(base)/<m>/debug/.cargo-lock`** (and on
  every other profile lock present, see below). Cargo takes this lock while
  it builds the debug profile there. Acquired and
  retained for the whole reclamation → the payload may go. Held → withheld.
  Missing anchor or failed probe → `ProbeFailed`, withheld. This is the
  existing `L` axis, applied to the resolved anchor (which is what the
  current probe already opens when a link is present).

What cosmon does **not** use:

* **Command-line arguments that contain the path.** This is the reporter's
  `du` trap, and it fails in both directions. A `du`, `find` or shell whose
  arguments mention the path is not writing into it, so argv presence is not
  evidence of use; and a compiler, linker or test binary running with its
  cwd or open files under the path need not mention it in argv, so argv
  absence is not evidence of non-use. A test that is neither necessary nor
  sufficient is not a test.
* **Recency (mtime).** Skipping "anything touched recently" is a proxy for
  "in use". The flock answers that question directly where it applies, and
  an mtime veto would withhold idle directories for no reason the flock
  does not already give.

What the build lock does not cover (measured, cargo 1.100.0, scratch
crate): Cargo locks **per profile directory**, not per target directory.
With `target/debug/.cargo-lock` held by another process, `cargo build`
and `cargo build --target aarch64-apple-ios-sim` blocked, but `cargo build
--release` ran to completion and wrote `target/release/`. So the anchor
named in `LOCK_ANCHOR` excludes debug and cross-target builds and does not
exclude a release build. This gap exists today for the in-worktree rule as
well; it is recorded here, not fixed. For external roots the implementing
molecule must acquire every `<profile>/.cargo-lock` and
`<triple>/<profile>/.cargo-lock` present under `canonical(base)/<m>` and
preserve each as an anchor; a profile whose lock file does not yet exist
cannot be locked in advance, so a build of a new profile that starts during
reclamation is not excluded. For a terminal molecule the residual cost of
that race is a rebuild, which is the derived-tier cost ADR-178 accepts.

What cosmon could observe but does not rely on: a scan of live processes'
cwd and open file descriptors under the path (`/proc/*/cwd` and `/proc/*/fd`
on Linux, `lsof` on macOS). It is a snapshot, the same limitation ADR-178
§3 names for `git status`: a process can open a file after the scan. It is
not an exclusion and cannot replace the flock. Whether to add it as an
*extra veto* is open question Q1.

*Failure prevented:* deleting a build that is running (flock held), and
being blocked forever by an unrelated reader (argv matching).

### D6 — Orphaned external directories are enumerated from the base

Because `cs done` removes the link and leaves the external directory, the
pass also enumerates each declared base: every direct child
`canonical(base)/<name>` that is not already reached through a live
worktree link becomes an **orphan candidate**. An orphan is selected only if
all of the following hold:

1. `<name>` parses as a molecule id;
2. that molecule's record is Present in this fleet's store and its status
   is `Completed` or `Collapsed` (D4; `M = Absent` withholds — the child may
   belong to another fleet sharing the base). `cs done` keeps the molecule
   record: a harvested molecule still loads with status `completed` (observed
   on `task-20261115-cd0e`). If an operator archives or deletes records, the
   orphans of those molecules read `M = Absent` and are withheld — the safe
   direction, reported with that reason;
3. the flock on `canonical(base)/<name>/debug/.cargo-lock` is acquired and
   retained (D5);
4. the enumeration of the base and of the worktrees is complete (D2).

It is then reclaimed as in D3. Children that are not directories, are
symbolic links, or fail any condition are reported as withheld with the
reason. The base is read, never written, except under a selected child.

*Failure prevented:* the reporter's main leak — external build directories
that nothing can see once their worktree is harvested.

### D7 — The layout is observed, not assumed (the `readlink` trap)

For each candidate and each root with a declared base, the adapter reads
the entry with `lstat` and classifies it:

| `w/<root>` is | Rule applied |
|---|---|
| a real directory | the existing in-worktree rule, unchanged |
| a link resolving to `canonical(base)/<m>` | D2–D5 |
| a link resolving anywhere else | refused with the resolution in the reason |
| a dangling link | no root from `w`; reported with a reason rather than skipped silently |
| absent | no root from `w` |

When a worker replaced the link with a real directory, the in-worktree rule
reclaims `w/target`, and any stale `canonical(base)/<m>` left from before is
reached as an orphan under D6, with the same gate and lock. Neither path
assumes the other exists.

Today one escaping root makes the whole candidate `Unavailable` by early
return. Under this ADR, a refused external root is reported per root and
does not prevent the candidate's other validated roots from being selected.

### D8 — No new verb; nothing new becomes automatic

The rule lives inside `cs purge --worktrees`. ADR-052 §D3 makes `cs purge`
the one infrastructure-teardown verb, and the module doc of
`worktree_reclaim.rs` records why a second entry point to the same safety
question is refused: the repository already drifted once by answering it in
two places. A `cs gc` verb would be a third copy.

Execution keeps the existing gesture: dry by default, removal only with
`--allow-unharvested` and without `--dry-run`. Reports name each external
root and orphan with its resolved path and its size, measured on the
resolved directory, so the register no longer under-reports this layout.

`cs tackle --reclaim-derived` calls the same pass with `execute = true`. It
will therefore reclaim declared external roots as well. That is consistent
with ADR-178 D1 — every automatic mechanism reclaims validated derived roots
and returns the worktree — because a declared external root is a validated
derived root and the worktree, including its link, is untouched. It remains
opt-in per dispatch; nothing runs on a timer.

Reclaiming the harvested molecule's external payload inside `cs done` would
also be compatible with ADR-178 (derived bytes, one named molecule, an
operator gesture), but this ADR does not adopt it; see alternative 2.

### D9 — Out of scope

* **Simulator devices.** They are owned by `simctl` (or the platform's
  equivalent), not by files under a declared base; deleting one is a call
  into another tool's state, and cosmon holds no lock that excludes the
  simulator service. No rule as safe as D1–D5 can be stated for them here.
* **Per-molecule temporary directories outside a declared base.** Not
  touched. A project may declare another external root for them, but D5's
  exclusion is Cargo's build lock, which excludes Cargo and nothing else.
  The adapter already withholds an in-worktree root that does not contain
  the lock anchor (`exclusion_established`); an external root without the
  anchor is withheld for the same reason until an exclusion protocol for it
  exists.
* **The worktree itself.** ADR-178 is unchanged.

---

## 3 · Open questions

**Q1 — Should a cwd/open-file scan be added as an extra veto?**
(a) No: the flock is the exclusion; a scan adds latency (`lsof` on macOS is
slow on large trees) and failure modes for no exclusion guarantee.
(b) Yes, as a veto only: an observed cwd or open file under the path
withholds; a failed scan is `Unknown` and also withholds. This catches a
non-Cargo process (a debugger holding a binary open) that the flock does not
exclude, at the cost of transient withholds when a `du` happens to be
running. Never (c): a clean scan as permission. Recommendation: (a) for the
first increment, revisit if a non-Cargo writer into a Cargo build directory
is observed.

**Q2 — May the residual skeleton of an orphan be removed?** D3 leaves
`<m>/debug/.cargo-lock` and its parent. For an orphan whose worktree is
gone, a second step could remove the skeleton once no worktree links to it.
The bytes are negligible; the risk is the inode race the anchor rule
exists for. Default: leave it.

**Q3 — Should cosmon create the layout?** Today the link is created by the
reporter's tooling, so D7 observes it. If `cs tackle` created
`w/<root> → base/<m>` when a base is declared, the layout would be a
contract rather than an observation, and the `readlink` trap would only
remain for workers that rewrite it. Not decided here.

**Q4 — Depth of the per-molecule directory.** D2 requires the link to
resolve to exactly `<base>/<m>`. Issue #218 says only that each link points
to "a per-molecule directory on another volume"; it does not give the depth.
If a layout uses `<base>/<m>/target`, D2 refuses it. Options: (a) keep the
exact child, and have the project choose its base accordingly; (b) declare
a template, e.g. `"{base}/{molecule}/target"`, still requiring an exact
match and the molecule id as a path component. Default: (a); (b) if a real
layout needs it. Either way the molecule id must be a component and the
match exact, so D2's sharing argument holds.

---

## 4 · Acceptance criteria — refutation tests

Each line is a test the implementing molecule must write, phrased as the
outcome that would refute this ADR.

1. No base declared, `w/target → elsewhere`: `Unavailable`, `elsewhere`
   untouched (`symlinked_derived_root_is_unavailable` passes unchanged).
2. Base declared, `w/target` → a directory outside the base: refused,
   target untouched, reason names the resolution.
3. `w/target → <base>` (the base itself): refused, nothing under the base
   touched.
4. `w/target → <base>/<other-id>`: refused, `<base>/<other-id>` untouched.
5. Two candidates whose links resolve to the same directory: neither
   selected, directory untouched.
6. Enumeration incomplete (an injected `enumeration_errors` entry): no
   external root and no orphan selected; in-worktree roots unchanged.
7. Molecule `Running`, `Pending`, `Queued`, `Frozen`, `Starved`, or status
   `Unknown`: external payload untouched.
8. Molecule record Absent, `w/target → <base>/<dir-name>`: external payload
   untouched; in-worktree behaviour unchanged.
9. A second process holds the flock on `<base>/<m>/debug/.cargo-lock`:
   `Held`, nothing removed. Same with the flock held on
   `<base>/<m>/release/.cargo-lock` only.
10. Anchor missing under `<base>/<m>`: `ProbeFailed`, nothing removed.
11. A `du` child process walks `<base>/<m>` without taking the flock, the
    flock is acquired: the payload is removed (argv is not consulted). The
    same with the flock held by the second process: nothing removed.
12. Real directory at `w/target` and a stale `<base>/<m>`: `w/target`
    payload reclaimed under the in-worktree rule; `<base>/<m>` reached only
    as an orphan and only if D6 holds.
13. Orphan `<base>/<m>` with `m` Completed and the flock acquired: payload
    removed, anchor and its parent preserved, base and siblings untouched.
14. Orphan `<base>/<name>` where `<name>` is not a molecule id, or names a
    molecule absent from this store: untouched, reported withheld.
15. A link inside the payload pointing outside it: the link is unlinked,
    its target untouched.
16. The link is swapped to another directory between observation and
    execution: execution refuses, nothing removed under either directory.
17. The link `w/target` still exists after execution.
18. Invalid base (relative, `/`, home directory, repository root, an
    ancestor of the repository root, inside a worktree): the pass refuses
    to run and names the condition.
19. Dry run and the absence of `--allow-unharvested`: nothing removed.
20. `python3 tests/harness/worktree-reclaim-contract.py` stays GREEN with
    the seven-axis table unchanged.

---

## 5 · Alternatives considered

1. **The reporter's `cs gc`.** A new verb that removes worktrees, build
   directories, simulator devices and temporary directories of terminal
   molecules. Rejected: it adds a verb ADR-052 §D3 refuses, and it reaches
   whole worktrees, which ADR-178 D1 forbids for any automatic path and D2
   forbids from durable eligibility. The build-directory half is what this
   ADR adopts, inside `cs purge --worktrees`.
2. **Automatic reclaim at `cs done`.** Compatible with ADR-178: the bytes
   are derived and the gesture names one molecule. Not adopted in this
   increment: the harvest transaction lands work on `main`, and adding a
   destructive filesystem step to it widens what can go wrong in the gesture
   that must not fail half-way. D6 reaches the same directories one
   `cs purge --worktrees` later. Worth reconsidering once D6 has run on a
   real fleet.
3. **Leave it to operator tooling and document the layout.** The cheapest
   option, and the reporter's current state. Rejected because the reporter's
   two traps are exactly the errors each operator's loop reinvents, and the
   orphan after `cs done` is invisible to every cosmon register: the
   withheld report under-reports these bytes and never names the orphans.
4. **A disk budget with eviction (issue #61's second proposal).** Rejected
   again for the reasons ADR-178 §4 gives: a byte threshold that triggers
   removal is a destructive action decided by a number, not by an observed
   exclusion, and it would select by size rather than by terminal status.
   Reporting sizes so an operator can decide stays in scope (D8).

---

## 6 · Consequences

* A project with external build directories gets reclamation by adding one
  declaration; a project without one sees no change.
* The withheld register reports external bytes and orphans, so the
  operator can see what a `--allow-unharvested` run would free.
* The contract gains a subsection for external roots and orphan candidates;
  its seven-axis table and witness are unchanged.
* `cs tackle --reclaim-derived` reclaims declared external roots as well.
* Simulator devices, undeclared temporary directories and whole worktrees
  stay out of every automatic and operator path in cosmon.
