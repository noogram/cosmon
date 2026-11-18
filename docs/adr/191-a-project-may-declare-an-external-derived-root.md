<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ADR-191 — A project may declare an external derived root; `cs purge --worktrees` reclaims it for terminal molecules

**Status:** Accepted (2026-11-18). Proposed 2026-11-16; amended 2026-11-18.
**Date:** 2026-11-16.
**Decider:** Noogram.
**Authoring molecule:** `task-20261116-ac57`.
**Amending molecule:** `task-20261118-b7d7`.
**Answers:** GitHub issue #218 (second half), follow-up to issue #61.

> **Amendment 2026-11-18 — several bases and a fixed suffix after the
> molecule id.** The first text allowed one external base per derived root
> and required a link to resolve to exactly `canonical(base)/<m>`; Q4 left
> the shape of the per-molecule directory open. The reporter has since
> described their layout (§1.1): the per-molecule directory is
> `<base>/<m>-wt`, the link points at that directory itself, and older and
> newer molecules live under two bases on two volumes. Under the first text
> neither part is reclaimable. This amendment changes:
>
> * **D1** — a root may be declared several times, one entry per base, each
>   with an optional fixed `suffix` (default empty) that follows the
>   molecule id; the suffix is validated as a single-name fragment; a
>   duplicate (same canonical base, same suffix) and a base nested inside
>   another declared base are configuration errors; a base that cannot be
>   reached is reported and skipped instead of stopping the pass.
> * **D2** — the link must resolve to exactly `canonical(base_i)/<m><suffix_i>`
>   for one declared entry `i`; children are classified once per canonical
>   base, and a name that reads as a molecule id under more than one entry
>   is withheld as ambiguous; uniqueness is checked across all entries, on
>   canonical path and on device and inode (a bind mount defeats a path
>   comparison alone).
> * **D3, D5, D6, D7, D8** — "the base" and `<m>` become "the entry's base"
>   and `<m><suffix>`; the orphan scan covers every declared base and
>   reports which base each item belongs to; an unreachable base never
>   affects another.
> * **Q4** is closed (one level, id plus fixed suffix); a deeper template
>   stays refused.
> * **§4** gains criteria 21–28; criteria 1–20 are unchanged and read with
>   an empty suffix. **§5** gains alternatives 5 and 6.
>
> Everything else, including the gate, the lock rule and the scope, is the
> 2026-11-16 text.

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

On 2026-11-17 the reporter added, on the issue, the shape of their layout
(this paragraph records what they wrote; the decision is in §2):

* the per-molecule directory is named `<molecule-id>-wt` — the molecule id
  followed by a fixed `-wt` — and the `target` link points at that
  directory itself, with no `target` below it;
* there are two bases on two volumes: older molecules point into a base on
  one external disk, newer ones into a base on a second disk added during a
  night when the first filled and slowed down;
* apart from this, each worktree's `target` is a per-molecule child of a
  base on another volume.

They said either of two forms would work for them: (a) the ADR accepts
several declared bases and an optional fixed suffix after the id, or (b)
they drop the suffix for new molecules and clean up old ones by hand. They
preferred (a) as the more general form, since a project that outgrows one
disk mid-run meets a second base as well. This amendment adopts (a).

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

A project may declare, per derived root, one or more **external entries**.
An entry names an **external base** — an absolute directory under which
per-molecule copies of that root live — and an optional fixed **suffix**
that follows the molecule id in the per-molecule directory's name (default:
empty). Several bases for one root are declared by repeating the root, one
entry per base. Proposed spelling (final spelling is the implementing
molecule's):

```toml
[worktree_reclaim]
evict = ["target"]

[[worktree_reclaim.external]]
root = "target"                       # must also appear in `evict`
base = "/Volumes/Builds-A/cosmon-target"
suffix = "-wt"                        # optional; default ""

[[worktree_reclaim.external]]
root = "target"
base = "/Volumes/Builds-B/cosmon-target"
suffix = "-wt"
```

With these two entries, molecule `m` may own `/Volumes/Builds-A/cosmon-target/<m>-wt`
or `/Volumes/Builds-B/cosmon-target/<m>-wt`, and nothing else under either base.

Every entry is validated when the pass starts, like `evict` today. The
conditions on a base are the 2026-11-16 ones and apply to each entry: it
must be absolute; its canonical form must not be `/`, the user's home
directory, the repository root, an ancestor of the repository root, or a
path inside the repository or any worktree. The suffix must be a plain
fragment of one path component, so that `<m><suffix>` is always a single
direct child of the base: it may be empty; it must not contain `/`, `..`
or NUL; it is at most 64 bytes. Across entries: two entries whose bases
canonicalize to the same directory with the same suffix are a duplicate;
a base whose canonical form lies strictly inside another declared base is
nested (the outer base's scan would see the inner base as a child). An
entry that fails any of these conditions is a configuration error, refused
with the condition named, and the pass does not run.

Reachability is the one condition that changed. The 2026-11-16 text also
required the base to exist and canonicalize, or the pass would not run. A
project with two volumes will sometimes run the pass with one of them
unmounted, and stopping the whole pass then would hold back the reachable
base and every in-worktree root for a disk that is simply absent. So an
entry whose base does not exist or does not canonicalize at pass start is
**unreachable**: it is reported, with the base and the error, and skipped
for this pass — no link is accepted into it and its orphan scan does not
run. The conditions that need a canonical form (the forbidden locations,
duplicates, nesting) are checked on every entry that canonicalizes; an
unreachable entry is checked lexically, and since nothing under it is
touched, a condition left unchecked on it cannot cause a deletion. An
unreachable base is a per-base condition, not an enumeration error of the
worktree population; D2 says why it does not weaken uniqueness.

Without a declaration, nothing changes: a link that leaves the worktree is
`Unavailable`, exactly as today, and `symlinked_derived_root_is_unavailable`
keeps passing unchanged.

*Failure prevented:* deleting through an arbitrary link target. Only a
directory the project named in advance can ever hold reclaimable external
bytes.

### D2 — The link target must be the molecule's own child of a declared base

For a candidate worktree `w` owned by molecule `m`, a link at `w/<root>` is
an external derived root only if its canonical target is **exactly**
`canonical(base_i)/<m><suffix_i>` for one reachable entry `i` declared for
`<root>` — a direct child of that entry's base whose name is the molecule
id followed by that entry's suffix. Any other resolution is refused with a
reason and selects nothing:

* resolves outside every declared base → refused (as today);
* resolves to a base itself → refused (a shared, persistent target);
* resolves to a child of a base whose name is not `<m><suffix_i>` for an
  entry `i` of that base — `<other-id><suffix_i>`, or `<m>` alone when the
  only suffix declared there is `-wt`, or `<m><suffix_j>` for a suffix
  declared on a different base → refused (another molecule's directory, or
  not a layout any entry names);
* resolves deeper, to `canonical(base_i)/<m><suffix_i>/sub` → refused (not
  the layout the declaration names).

The molecule id component is compared, not inferred: the child name must be
`<m><suffix_i>` with `m` the candidate's own molecule id. A link whose name,
once the suffix is removed, is a different molecule id is refused. The
suffix never lets one molecule claim another's directory.

**One child name, one reading.** A molecule id's last segment is any
non-empty ASCII alphanumeric run, and its prefix may contain hyphens
(`MoleculeId::parse_inner`, `crates/cosmon-core/src/id.rs`). So when one
canonical base carries several suffixes, one child name can read as two
molecules: with suffixes `""` and `t`, `task-20261118-b7d7t` is molecule
`task-20261118-b7d7t` under the first entry and `task-20261118-b7d7` plus
`t` under the second. Children are therefore classified once per canonical
base, against every entry of that base: a name that yields a valid molecule
id under exactly one entry is a candidate for that entry; a name that
yields one under none, or under more than one, is withheld with the reason
("not a molecule directory" or "ambiguous between entries `i` and `j`").
D2 accepts a link only into a child classified for the candidate's own
molecule; D6 uses the same classification.

In addition, a resolved external directory must be **unique across a
complete enumeration**: if two candidates' links resolve to the same
directory, neither is selected, whichever declared base or bases they
reached it through. The check runs on canonical paths across all entries
together, not per entry: two bases declared as different spellings of the
same directory (a path through a symbolic link) collapse to one canonical
base before any comparison. A bind mount is different: it makes one
directory reachable under two canonical paths, which no path comparison
sees. The uniqueness check therefore compares the resolved directories'
device and inode numbers as well as their canonical paths; the duplicate
and nesting checks of D1 compare paths only, so two declared bases that
are the same directory through a bind mount are not detected there, and
the uniqueness check is what refuses a directory reached through both. The
naming rule
already makes a collision impossible for well-formed links; the cross-check
is the defence for everything the naming rule assumes. Uniqueness can only
be established over a complete population, so a pass with non-empty
`enumeration_errors` selects **no** external root (in-worktree roots keep
their current behaviour).

An unreachable base (D1) is not such an error. A link into an unmounted
base does not resolve, so it is dangling (D7) and selects nothing; it
cannot resolve to the same canonical directory as a link into a reachable
base, because reachable bases are pairwise equal-or-disjoint (no nesting)
and a directory under one cannot also be under another. The population
that uniqueness ranges over — links that resolve — is therefore complete
while a base is absent, and the reachable bases keep their selection.

*Failure prevented:* deleting a directory shared by two molecules, or a
persistent target such as a global `CARGO_TARGET_DIR` placed at the base
itself.

### D3 — What is deleted and what is never deleted

Below, `E = canonical(base_i)/<m><suffix_i>` is the external directory D2
accepted, for the entry `i` it was accepted under.

**Deleted:** the payload of `E`, using the same walk as
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
* every declared base directory, and anything under any base other than
  `E`;
* anything reached only through a link inside the payload.

Execution re-resolves the link under the held lock and refuses if the
canonical target differs from the one observed (a link swapped between
observation and execution), including a target that is still a
well-formed molecule directory but under another entry or another base. The dry-run observation is not mutation
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

* **An acquired flock on `E/debug/.cargo-lock`** (and on
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
`<triple>/<profile>/.cargo-lock` present under `E` and
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
pass also enumerates **every** reachable declared base, once per canonical
base: every direct child `canonical(base)/<name>` that is not already
reached through a live worktree link (decided with D2's identity:
canonical path, device and inode) becomes an **orphan candidate**. The
child is classified as in D2: for each entry `i` of that base such that
`<name>` ends with `suffix_i` (an empty suffix always does), the reading
under `i` is `<name>` with `suffix_i` removed from the end. A
child that ends in no declared suffix of its base, whose stripped name is
not a molecule id under any entry, or that reads as a molecule id under
more than one entry, is reported withheld with that reason. An orphan
`canonical(base_i)/<m><suffix_i>` is selected only if all of the following
hold:

1. `<name>` classifies under exactly one entry `i` of its base, to molecule
   id `m`;
2. that molecule's record is Present in this fleet's store and its status
   is `Completed` or `Collapsed` (D4; `M = Absent` withholds — the child may
   belong to another fleet sharing the base). `cs done` keeps the molecule
   record: a harvested molecule still loads with status `completed` (observed
   on `task-20261115-cd0e`). If an operator archives or deletes records, the
   orphans of those molecules read `M = Absent` and are withheld — the safe
   direction, reported with that reason;
3. the flock on `canonical(base_i)/<name>/debug/.cargo-lock` is acquired
   and retained (D5);
4. the enumeration of the worktrees is complete (D2), and the listing of
   this base was read without error.

It is then reclaimed as in D3. Children that are not directories, are
symbolic links, or fail any condition are reported as withheld with the
reason. Every report line for an orphan, selected or withheld, names the
base it was found under. A base is read, never written, except under a
selected child.

Bases are independent. An unreachable base (D1) is reported once, with its
path and the error, and its scan does not run. A reachable base whose
listing fails part-way withholds every child of that base, with the error.
Neither condition touches anything, and neither causes another base's scan
to be skipped or shortened: each reachable base is scanned and reported on
its own line, and a base that was not scanned says so rather than
appearing as a base with no orphans.

*Failure prevented:* the reporter's main leak — external build directories
that nothing can see once their worktree is harvested.

### D7 — The layout is observed, not assumed (the `readlink` trap)

For each candidate and each root with at least one declared entry, the
adapter reads the entry with `lstat` and classifies it:

| `w/<root>` is | Rule applied |
|---|---|
| a real directory | the existing in-worktree rule, unchanged |
| a link resolving to `canonical(base_i)/<m><suffix_i>` for a reachable entry `i` | D2–D5 |
| a link resolving anywhere else | refused with the resolution in the reason |
| a dangling link (including a link into an unreachable base) | no root from `w`; reported with a reason rather than skipped silently |
| absent | no root from `w` |

When a worker replaced the link with a real directory, the in-worktree rule
reclaims `w/target`, and any stale `canonical(base_i)/<m><suffix_i>` left
from before, under any declared base, is reached as an orphan under D6,
with the same gate and lock. Neither path assumes the other exists.

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
root and orphan with its resolved path, the declared base it lies under,
and its size, measured on the resolved directory, so the register no
longer under-reports this layout. Unreachable bases are reported in the
same register.

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

**Q4 — Depth of the per-molecule directory. Closed 2026-11-18.** The
2026-11-16 text required the link to resolve to exactly `<base>/<m>` and
asked whether a layout might need another shape. The reporter's answer
(§1.1) is `<base>/<m>-wt`: one level, the molecule id followed by a fixed
string. Decision: the per-molecule directory is a direct child of a
declared base, named the molecule id followed by the entry's fixed suffix
(possibly empty), matched exactly (D1, D2). The molecule id remains the
leading part of a single path component and the match remains exact, so
D2's sharing argument holds.

Still open: a deeper layout such as `<base>/<m>/target`, or any other
fixed template with more than one component or text before the id, is
refused. A template could be added later, under the same requirements
(exact match, molecule id compared rather than inferred, one reading per
name), if a real layout needs it; none does today.

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

Criteria 1–20 read with one declared entry and an empty suffix. The
2026-11-18 amendment adds the following; `<base1>` and `<base2>` are
canonical, disjoint, and both declared for `target` unless the line says
otherwise.

21. `<base2>` declared with suffix `-wt`, `w/target → <base2>/<m>-wt`, `m`
    Completed, flock acquired: payload removed, report names `<base2>`.
22. Only `<base1>` declared, `w/target → <base2>/<m>-wt`: refused as
    outside every declared base, `<base2>/<m>-wt` untouched.
23. Suffix `-wt` declared, `w/target → <base1>/<m>`: refused,
    `<base1>/<m>` untouched.
24. Suffix `-wt` declared, `w/target → <base1>/<other-id>-wt` with
    `other-id` a valid, Completed molecule of this store: refused,
    `<base1>/<other-id>-wt` untouched.
25. The same directory reached from two candidates through two declared
    bases: two candidates owned by the same molecule `m` (for example a
    `.worktrees/` directory and a stale registered worktree of `m`, or an
    injected owner in the observer fixture), `w1/target → <base1>/<m>-wt`
    and `w2/target → <base2>/<m>-wt`, where `<base2>/<m>-wt` is itself a
    symbolic link to `<base1>/<m>-wt`. Both links canonicalize to
    `<base1>/<m>-wt`: neither candidate selects it, the directory is
    untouched, and the symbolic-link child of `<base2>` is withheld by the
    orphan scan (D6). Distinct canonical bases cannot hold the same
    directory, so a link through a second base is the construction that
    exercises the cross-entry check.
26. Orphan `<base2>/<m>-wt` with no live worktree link: selected only with
    `m` Present, Completed or Collapsed, flock acquired and enumeration
    complete; with any of these missing it is untouched and reported
    withheld, naming `<base2>`. A child `<base2>/<m>` (no suffix) and a
    child `<base2>/notanid-wt` are reported withheld and untouched.
27. `<base2>` declared but absent (volume unmounted): the pass runs;
    `<base2>` is reported unreachable; an eligible orphan or linked root
    under `<base1>` is selected exactly as with `<base2>` absent from the
    configuration; a worktree whose link points into `<base2>` is reported
    as a dangling link.
28. Configuration errors, each refusing the pass with the condition named:
    a suffix containing `/`, `..`, or NUL, or longer than 64 bytes; two
    entries with the same canonical base and the same suffix; a base
    nested strictly inside another declared base. And one ambiguity, which
    is not a configuration error: one base declared with suffixes `""` and
    `t`, a child `task-20261118-b7d7t` with both readings valid molecule
    ids of this store — withheld as ambiguous, untouched, under D2 and D6.

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
5. **One base per root and an exact child named by the id (the 2026-11-16
   text).** The simplest rule to prove safe, and the one this ADR first
   proposed. Replaced on 2026-11-18 because it did not fit the one layout
   reported: the reporter's directories are `<m>-wt`, so none matched, and
   their second base, added when the first disk filled, could not be
   declared at all. A project that outgrows one volume mid-run meets a
   second base for the same reason. The amendment keeps what made the
   first rule safe — one level, exact match, the molecule id compared, a
   uniqueness cross-check — and widens only the name (a fixed suffix) and
   the number of bases.
6. **A free-form template language** (for example
   `"{base}/{prefix}-{molecule}/build/{profile}"`). More general: it would
   cover deeper layouts and text before the id. Not adopted: each template
   feature must be shown not to let one molecule's link match another's
   directory, not to match a shared directory, and to give one reading per
   path, and that proof grows with the language. A fixed suffix on one
   component has a short proof (D2). Q4 keeps a template open for a real
   layout that needs one.

---

## 6 · Consequences

* A project with external build directories gets reclamation by adding one
  declaration per base; a project without one sees no change.
* A base on an unmounted volume is reported and skipped; it never stops
  the pass or another base.
* The withheld register reports external bytes and orphans, so the
  operator can see what a `--allow-unharvested` run would free.
* The contract gains a subsection for external roots and orphan candidates;
  its seven-axis table and witness are unchanged.
* `cs tackle --reclaim-derived` reclaims declared external roots as well.
* Simulator devices, undeclared temporary directories and whole worktrees
  stay out of every automatic and operator path in cosmon.
