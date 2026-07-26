# Issue #20 — which verdict speaks for the current head

**Read this first.** The mission on `noogram/cosmon#20` pronounced twenty-one
gate verdicts over three days. Several of them say `BLOCKED`. None of them is
wrong. Every one is a true measurement *of the bytes it was pronounced on* —
and most of those bytes have since been replaced.

This file says, for the frozen head, which verdict is current and which is not.
It projects that from [`succession-register.jsonl`](succession-register.jsonl)
and [`verdict-catalogue.jsonl`](verdict-catalogue.jsonl); it invents nothing and
it changes nothing. **No verdict was modified to produce it** — not even to add
a field. A verdict written yesterday cannot know the one that overtakes it
today, so the link between them belongs to neither: it belongs to the history,
and the history is a separate, append-only file.

---

## The frozen head

```
9e2a8ce2cea202bba55966ffe16acfc7aa9151a4   Merge branch 'feat/task-20260725-a088'
```

on the integration trunk `feat/container-worker-doors` — **not** `main`.
Nothing was pushed.

The commit that divides "before" from "after" is the last door-4 code fix:

```
73c4b2a   fix(readiness): the dispatch gate is an allow-list, not a deny-list
```

A verdict pronounced *before* `73c4b2a` was measuring a binary in which door 4
was still open. It remains true about that binary. It does not describe this
one.

---

## Authoritative for this head

One verdict, on one subject.

| subject | verdict | pronounced on | where |
|---|---|---|---|
| `door-4-differential` | arm C **NOT PROVEN** on `4c41738`, **PROVEN** on `73c4b2a` | `73c4b2a` | [`docs/benches/issue-20-door-4-differential.md`](../../benches/issue-20-door-4-differential.md) |

That is the whole authoritative set. It is a short table, and the shortness is
the finding: see [*The thirteen holes*](#the-thirteen-holes) below.

---

## Superseded — true then, not current now

Seven transitions. Each names the verdict that overtook it and why. The old
file is untouched and still readable at its own path; follow the `next` column
to reach the current word on the same question.

| # | subject | superseded verdict | pronounced on | superseded by | pronounced on |
|---|---|---|---|---|---|
| SR-001 | `g1-behavioural-contract` | `task-20260723-5371/verdict.json` — BLOCKED | *indeterminate* (v0.2.2 line) | `task-20260725-9a44/contract-verdict.json` — PASS | *indeterminate* (trunk) |
| SR-002 | `repository-gates` | `task-20260723-c710/verdict.json` — BLOCKED | `f002da8` | `task-20260725-97da/verdict.json` — PASS | `a07cabf` |
| SR-003 | `cross-provider-referee` | `task-20260723-b0a5/verdict.json` — CLEAN | `f002da8` | `cmbverify-20260725-ed95/verdict.json` — FINDINGS (7) | `a07cabf` |
| SR-004 | `cross-provider-referee` | `cmbverify-20260725-ed95/verdict.json` — FINDINGS (7) | `a07cabf` | `cmbverify-20260725-186c/verdict.json` — FINDINGS | `97e7eeb` |
| SR-005 | `committee-adjudication` | `task-20260723-631e/verdict.json` — inconclusive | `f002da8` | `task-20260725-f02f/committee-verdict.md` — inconclusive-by-missing-witness | `afb5541` |
| SR-006 | `committee-adjudication` | `task-20260725-f02f/committee-verdict.md` | `afb5541` | `task-20260725-1e40/committee-verdict.md` — inconclusive | `97e7eeb` |
| SR-007 | `cmb-verify-mechanism` | `cmbverify-20260725-ed95/verdict.md` — confirmed | `a07cabf` | `cmbverify-20260725-186c/verdict.md` — confirmed | `97e7eeb` |

`SR-003` and `SR-004` supersede the same file twice over. That is the register
working as designed: a second supersession is a **second entry**, never a
rewrite of the first.

**`f002da8` and `4e57b19` are not ancestors of the frozen head.** The whole
round-1 line of 2026-07-23 was abandoned rather than merged. Verdicts
pronounced there measured bytes that are not in this history at all — a
stronger form of staleness than "superseded", and one no reader would guess
from the files.

---

## The thirteen holes

Thirteen of the fourteen subjects have **no** verdict that speaks for the frozen
head. Their last word predates `73c4b2a`, and nobody re-pronounced them after
the fix landed.

| subject | last word | verdict | pronounced on | why it is not current |
|---|---|---|---|---|
| `g0-intake` | `task-20260723-5be4/verdict.json` | BLOCKED | *indeterminate* | subject sha unknown; v0.2.2 line |
| `g1-behavioural-contract` | `task-20260725-9a44/contract-verdict.json` | PASS | *indeterminate* | subject sha unknown |
| `g2-reproduce` | `repro-20260723-a38d/verdict.json` | BLOCKED | `9639f85` | before the fix |
| `g3-falsify` | `task-20260723-d1b2/verdict.json` | PASS | `7aeb6f2` | before the fix |
| `g4-implementation` | `task-20260723-d94b/verdict.json` | BLOCKED | `f002da8` | abandoned line |
| `g5-green-on-original-red` | `task-20260723-f18f/verdict.json` | BLOCKED | `4e57b19` | abandoned line |
| `repository-gates` | `task-20260725-97da/verdict.json` | PASS | `a07cabf` | before the fix |
| `cross-provider-referee` | `cmbverify-20260725-186c/verdict.json` | FINDINGS | `97e7eeb` | before the fix |
| `committee-adjudication` | `task-20260725-1e40/committee-verdict.md` | inconclusive | `97e7eeb` | before the fix |
| `convergence` | `converge-20260723-a767/converge-verdict.json` | BLOCKED | `f002da8` | abandoned line |
| `cmb-verify-mechanism` | `cmbverify-20260725-186c/verdict.md` | confirmed | `97e7eeb` | before the fix |
| `g10-release-manifest` | `task-20260725-3866/verdict.json` | BLOCKED | `88e4ca8` | before the fix |
| `g11-closure` | `bug-closure-20260725-8c79/verdict.md` | REOPEN_WITH_MISSING_SURFACES | `88e4ca8` | before the fix |

A hole is not a contradiction and it is not a licence. `g11-closure` still says
**reopen**; nothing here says the issue is closed. What the hole says is
narrower and more useful: *no gate has been re-run since the fix, so the dossier
cannot claim the fix passes them.* Reading a hole as a green is the mistake this
index exists to make impossible.

This is a **finding, not an arbitration**. The mission that produced this index
was told not to pick winners, and it has not: filling a hole means re-running
the gate, which is a dispatch decision for the operator.

---

## How a reader gets from anywhere to here

Four entry points, four routes, no dead ends. All four are mechanical.

1. **From an old verdict.** You are holding `task-20260723-5371/verdict.json`,
   which says `BLOCKED`. Run:

   ```sh
   scripts/check-verdict-provenance.py --resolve task-20260723-5371/verdict.json
   ```

   It walks the register forward, prints every hop, and ends on the terminal —
   stating whether that terminal is authoritative or itself a hole. It never
   stops on a pointer to nowhere; condition (b) below proves that for all
   twenty-one verdicts at once.

2. **From a register entry.** Every entry names `previous` and `next` as
   catalogue paths. Feed either to `--resolve`.

3. **From the catalogue.** Every verdict has an id, a subject, a path and the
   sha it was pronounced on. It is a flat file; grep it.

4. **From this index.** The tables above are the projection. They are generated
   from the same two files the checker reads, so an index that drifts from the
   register is a check failure, not a matter of opinion.

---

## The rule for verdicts written after this index

**A new verdict cites the one it supersedes.** This costs nothing and breaks
nothing: the new verdict is being written *now*, so it may name its predecessor
without anachronism and without mutating anything.

Concretely, a gate verdict on this mission should carry:

```json
"supersedes": "cmbverify-20260725-186c/verdict.json",
"supersedes_subject_sha": "97e7eeb"
```

and the author appends the matching line to `succession-register.jsonl`.

**None of the seven existing successions carries such a citation.** Two of them
gesture at their predecessor in prose — `task-20260725-1e40` calls itself
"round 2", `cmbverify-20260725-186c` records "round-1 F1 closed" — but neither
names a file a reader could open. That is precisely why the register exists and
why it is a separate file: the linkage had to be recorded *somewhere*, and the
one place it could not be recorded was inside the verdicts themselves.

---

## The check

```sh
scripts/check-verdict-provenance.py
```

Its recorded output is [`check-output.txt`](check-output.txt). Exit codes:

| code | meaning |
|---|---|
| `0` | all four conditions clean |
| `2` | (a), (b), (c) clean; (d) has holes or ambiguities — the current state |
| `1` | hard failure: a mutated verdict, a dangling pointer, a cycle, a fork |

`2` is the honest status of this dossier today, and it should stay `2` until
someone re-runs the gates. It must never become `1`.

| condition | what it forbids | result |
|---|---|---|
| (a) immutability | editing a verdict after it was pronounced | **PASS** — 21/21 byte-identical |
| (b) accessibility | a chain that ends on a pointer to nothing | **PASS** — all 21 reach a terminal |
| (c) acyclicity and termination | a loop, or a fork with two "current" answers | **PASS** — 21 chains, 14 terminals, no cycle, no fork |
| (d) unicity | two silent claimants, or a silent hole | 1 exact, **13 holes**, 0 ambiguities |

---

## Files

| file | what it is |
|---|---|
| `AUTHORITATIVE-INDEX.md` | this file — the projection, for humans |
| `verdict-catalogue.jsonl` | one line per verdict: id, subject, path, verdict, subject sha. Append-only |
| `succession-register.jsonl` | one line per transition: previous, next, both shas, the reason. Append-only |
| `frozen-head.json` | the head this projection is made against, and the authority rule |
| `verdict-fingerprints.sha256` | the digest each verdict must keep for ever |
| `immutability-before-after.md` | the same digests taken before and after this dossier was written, and their diff |
| `verdict-inventory.md` | how the list of verdicts was built, so its completeness can be re-checked |
| `check-output.txt` | the recorded output of the check |
| `../../../scripts/check-verdict-provenance.py` | the check itself |
