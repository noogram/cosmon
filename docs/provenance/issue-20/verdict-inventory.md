# How the list of issue-#20 verdicts was built

A verdict missing from an index is worse than no index at all: the index gives
an impression of completeness that the omission then quietly betrays. So the
sweep that produced [`verdict-catalogue.jsonl`](verdict-catalogue.jsonl) is
written down here, command by command, for someone who wants to redo it and
disagree.

## 1. The two places a verdict can live

1. **Molecule state directories** — `.cosmon/state/fleets/default/molecules/`,
   282 molecules at the time of the sweep. This is where gate workers write.
   It is runtime state and is deliberately **not** tracked by git.
2. **Versioned artefacts in the repository** — the small number of results that
   were published into `docs/`.

Both were swept. Nothing else can hold a verdict for this mission.

## 2. The sweep

**Step 1 — candidate files, by name.** Across all 282 molecules:

```sh
find . \( -iname '*verdict*' -o -iname 'release-manifest.json' \
          -o -iname 'green.md' -o -iname 'referee-report.md' \) -type f
```

74 files.

**Step 2 — candidate molecules, by content.** Name is not enough: a verdict may
sit in a file called something else, and a file called `verdict.json` may belong
to another mission. So the mission's molecules were found by content:

```sh
grep -rqi 'cosmon#20\|issue #20\|issue-20\|container-worker-doors\|door 4\|door-4\|#20\b'
```

**Step 3 — the union, then subtraction.** Files from step 1 inside molecules
from step 2. Then each was opened and its `issue` field read.

**Two distinct issue vocabularies name this same mission**, which is the trap in
this dossier: the 2026-07-23 wave writes `COSMON-DEV #20`, the 2026-07-25 wave
writes `noogram/cosmon#20`. Both carry the same reported symptom —
*claude adapter: bypassPermissions-as-root dies; interactive acceptEdits hangs*,
reported by @jdthaler against signed `v0.3.0`. A sweep keyed on either string
alone would have found half the dossier and believed it had found all of it.

Rejected at this step, having matched by name or by a stray `#20` but belonging
elsewhere: everything with `issue` = `COSMON-DEV #21` (adapter selection —
`task-20260723-2acf`, `-2fc9`, `-85c3`, `-9b27`, `-b2d0`, `converge-20260723-86c4`),
`COSMON-DEV #27` (OIDC — `task-20260724-29fe`, `-2bc7`, `-2bdb`, `-368d`), and
`repro-20260723-5038` (`resolve_adapter_selection`, issue #21).

**Step 4 — the repository.**

```sh
git ls-files | grep -i 'verdict\|provenance\|release-manifest'
```

One published verdict for this mission:
`docs/benches/issue-20-door-4-differential.md`, with its raw outputs under
`docs/benches/issue-20-door-4-differential/`.

## 3. What was catalogued, and what was not

**21 catalogued verdicts** (`V-01`…`V-21`) — see the catalogue.

**Companions, not verdicts.** Some molecules render the same verdict twice, or
attach its evidence. These are listed in the catalogue's `companions` field and
are deliberately *not* separate rows, because counting them twice would
manufacture the "two claimants" ambiguity that condition (d) exists to detect:

- `task-20260723-631e/committee-verdict.md` — prose of `V-09`
- `task-20260723-f18f/green.md` — evidence for `V-06`
- `task-20260725-3866/release-manifest.json` — the manifest `V-19` grades
- `cmbverify-*/referee-report.md` — the reasoning behind `V-14` / `V-16`
- `bug-closure-20260725-8c79/{coverage,surfaces}.md` — evidence for `V-20`
- the `run-1/2/3` directories under the differential — raw output for `V-21`

**One molecule, two subjects.** `cmbverify-20260725-ed95` and
`cmbverify-20260725-186c` each answer two different questions and write two
different files: `verdict.md` says *does the stated mechanism hold?*
(`confirmed`), `verdict.json` is the referee's own count (`FINDINGS`).
`ed95/verdict.md` states this itself: *"a confirmed diagnosis does not license
this merge."* They are catalogued as separate verdicts with separate subjects,
because collapsing them would let a `confirmed` stand in for a referee's seven
findings.

**Documented absences.** Five molecules of this mission completed without
shipping any verdict. They are recorded here rather than in the catalogue,
because an absent verdict has no bytes to be immutable about — but a reader must
not mistake the gap for an oversight in this sweep:

| molecule | formula | what is missing |
|---|---|---|
| `cmbverify-20260723-6604` | `cmb-verify` | the round-1 cross-provider seat: no `verdict.json`, no `referee-report.md`, no `falsification-attempt.md`. This absence is the *reason* `V-09` is `inconclusive` |
| `cmbverify-20260725-d540` | `cmb-verify` | contract only, no verdict |
| `cmbverify-20260725-fe08` | `cmb-verify` | contract and reproduction, no verdict |
| `repro-20260725-53f8` | `clean-room-repro` | no verdict |
| `converge-20260725-1224` | `converge` | no verdict — the 2026-07-25 convergence gate never pronounced, which is why `convergence`'s last word is still `V-10` from 2026-07-23 |

`merge-20260725-c320` (`merge-conflict`) is not a gate: it produced a conflict
map and a synthesis, no verdict.

## 4. Subject shas that could not be determined

Three verdicts name no commit. They are recorded as `null` **in the catalogue
and the register**, never inside the verdict — inventing a sha would give the
causal chain a solidity it does not have, which is worse than admitting the gap.

| verdict | what was tried |
|---|---|
| `V-01` `task-20260723-5be4/verdict.json` | grepped the verdict, `state.json` and `log.md` for any 7–40 hex run: none. The molecule branch `feat/task-20260723-5be4` no longer exists (harvested). The verdict names only `affected_ref: v0.2.2` |
| `V-02` `task-20260723-5371/verdict.json` | same three files, same absence, same missing branch. Names only `v0.2.2` and its surface |
| `V-11` `task-20260725-9a44/contract-verdict.json` | same sweep. Names its trunk (`feat/container-worker-doors`) and its surface, no commit. Its brief mentions `f9084041` as *context* for the reported symptom, which is not the tree the contract was written against, so it was not adopted |

`V-05` and `V-09` carry a sha that is *inferred*, not self-declared: `f002da8`
is named as the fixed tree by `V-08` and `V-10`, two verdicts that audited
`task-20260723-d94b` directly. The catalogue records the inference in
`subject_sha_note` so a reader can reject it.

Note that this uncertainty changes nothing about the outcome: `V-01`, `V-02`
and `V-11` are stale-unreplaced either way — `V-02` because the register
supersedes it, `V-01` and `V-11` because a verdict whose subject cannot be
located cannot be shown to speak for the frozen head.

## 5. Re-checking this sweep

The catalogue is a flat file and the checker reads it. To dispute completeness,
add a row and re-run:

```sh
scripts/check-verdict-provenance.py
```

A verdict added without a fingerprint fails condition (a). A verdict added
without its succession fails condition (d) as a new hole or a new ambiguity.
Neither can be added silently, which is the point.
