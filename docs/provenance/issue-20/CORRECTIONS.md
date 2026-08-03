# Issue #20 — forward corrections to the lineage's own account

**What this file is.** Three of the findings that came out of round 1 of the
convergence review land not on code but on *accounts of code*: a commit message
already on a release-bound head, and a `result.md` that lives in gitignored
molecule state. Neither can be fixed where it was written. Rewriting history to
make an old sentence true is the opposite of the discipline these findings
enforce — it would erase the evidence that the sentence was ever wrong.

So the repair ships forward, here, as its own commit: a dated register saying
plainly what an earlier account got wrong, what is true instead, and how that
was measured. Nothing was force-pushed, no commit was amended, and the
superseded text is still readable at its own path.

Append-only. A correction is never edited to reflect later knowledge; a later
correction is added below it.

---

## C-001 — the clippy caveat named the wrong scope, and named it by memory

**Corrects:** `task-20260728-3b44/result.md`, the caveat accompanying commit
`24ad753`.

**What it said.**

> `cargo clippy --workspace --all-targets` is red, but every site sits in a
> crate this change does not touch.

**What is true.** The second clause is false. Measured on the tree this
correction ships against, from a full captured log rather than from memory:

- `cargo clippy --workspace --all-targets` with no `-D warnings` exits **rc=0**,
  emitting **62 unique warning sites** across **7 crates**: `cosmon-cli`,
  `cosmon-core`, `cosmon-filestore`, `cosmon-rpp-adapter`, `cosmon-runtime`,
  `cosmon-state`, `cosmon-transport`. It is "red" only under `-D warnings`, and
  `--all-targets` is not part of the configured lint gate at all — that gate is
  `cargo clippy --workspace -- -D warnings`, which is green.
- Three of those sites are in `crates/cosmon-core/src/committee.rs` — **the
  principal file `24ad753` edited**, by roughly +150 lines. They are
  `clippy::doc_markdown` (`base_url` unbackticked) in a doc comment, at
  `committee.rs:4196:68`, `:4197:60` and `:4198:28` on this tree; the same three
  sat at `4190`–`4192` before this commit's own edits shifted them, and at
  `4186` on `1a719a3`, the parent.

**Why it is a low finding and not a regression.** The lint content is
pre-existing: the identical doc comment carries the identical warnings on
`1a719a3`. Nothing `24ad753` wrote introduced a site. What is wrong is the
sentence a reviewer relies on to decide the gate can be skipped — and it is
wrong in a specific, repeatable way: **the sweep was described by crate when it
was performed by memory.** A crate-level claim is checkable in one command; the
account asserted the conclusion of that command without running it.

**One correction in the earlier account's favour.** The same result.md named
`crates/cosmon-runtime/tests/tick_probe_running.rs:12` as a site, and the round-1
driver recorded it as measured-absent (`grep -c` returned 0 in the seat's own
run). The full log captured here **does** contain
`crates/cosmon-runtime/tests/tick_probe_running.rs:12:42`. The site is real; the
seat's run was truncated by fail-fast ordering, not fabricated. The driver was
right to report the absence as a measurement rather than as an accusation.

**The rule this leaves behind.** A caveat about a gate is re-derived from that
gate's captured output, and the sites are named by grepping the log — never by
recalling which crates a diff touched.

---

## C-002 — the "no other emitted string" sweep asserts a scope it was not run at

**Corrects:** commit message `24ad753`, R2 paragraph, final sentence; and
`task-20260728-3b44/result.md`'s description of the same sweep.

**What it said.**

> No other emitted string in the tree hardcodes an axis.

and, in result.md, that the change "swept the file", concluding about "the tree"
in the same breath — with the further claim that "the only remaining
`witness 1` / `witness 2` occurrences are doc comments and test comments".

**What is true.** The scope actually swept was the file, `committee.rs`. The
enumeration is false at tree scope. `git grep -n 'witness [0-9]'` over the tree
also returns, on the head this correction ships against:

| site | what it is |
|---|---|
| `.cosmon/formulas/cross-provider-committee.formula.toml:295,296` | emitted step prose, rendered into a worker briefing |
| `.cosmon/formulas/cross-provider-committee.formula.toml:314,471` | emitted acceptance prose, likewise |
| `crates/cosmon-cli/src/cmd/evolve.rs:1007` | a production line comment |

None of the five is a doc comment or a test comment.

**What survives.** The *substantive* claim does. Those literals are
definitional — they name the two axes in prose written for a human reader, with
no `SeatRejection` in scope to read a property off, so there is no `label()`
standing where a `witness_axis()` was available. The round-1 driver confirmed
independently that `label()` and `witness_axis()` are both property-derived and
that no other instance of the defect class exists in code. The defect corrected
here is not the conclusion; it is **a measurement that was claimed and not
taken**, at a scope one order wider than the one examined.

**The rule this leaves behind.** State the scope actually swept. If a claim is
made at tree scope, the sweep runs at tree scope, and anything excluded — the
formula corpus, generated files — is excluded *by name*, not by having been
overlooked.

---

## C-003 — a release-bound change reported five gates when its contract is seven

**Corrects:** commit message `24ad753`, final line.

**What it said.**

> Gates: check, test, clippy, fmt, doc green

**What is true.** The sentence is not false — all five named gates were green,
and the accompanying result.md did report all seven. But `CLAUDE.md` is explicit
that the cargo five are not the whole contract:

```text
python3 scripts/spdx-headers.py --check
scripts/publish.sh --check
```

both run in CI, are not subsumed by the five, and `publish.sh --check` is
required for release-bound changes specifically. A commit message is the durable
artefact the next commit in a lineage copies, and this lineage has now copied
several sentences forward; a five-gate line reads as the full contract to
whoever copies it next.

**The rule this leaves behind.** Name all seven, or write "the five cargo gates"
so the omission reads as an omission rather than as completeness.

---

## C-004 — the witness-2 damage reaches further back than the seats that found it

**Records** (rather than corrects) a systemic fact established by the round-1
driver, so that it is legible outside gitignored molecule state.

Until the fix that ships with this register, `cs complete` rewrote `briefing.md`
to a terse COMPLETED text without re-establishing a committee seat's pointer at
its durable `committee-posture.md` — while `cs tackle` and `cs evolve` both did.
`cs complete` is the verb every seat ends with, so persona witness (2) —
posture file present **and** briefing pointing at it — was true only while a
seat was running and false at every moment it could be audited.

Measured before/after on round 1's two seats, with both `cs reconcile --check`
runs captured to files before anyone knew what they would show: after `cs
tackle`, zero witness-2 lines and zero violations for `committee-20260728-2d37`;
after both seats completed, `fails witness 2 (briefing-not-injected)` and
`grep -c committee-posture.md briefing.md == 0` for **both** — though the lint
reports only one of the two, so its own output under-counts the damage.

The same line stands against `committee-20260728-49a3` / seat
`cmbverify-20260728-6e03` — the previous loop's floor-bearing seat, whose
delivery that loop's `converge-verdict.json` records as validity condition 3
MET. That record is not being altered here. What is recorded is that the witness
it rests on was, at the moment of reading, unsatisfiable by construction for
every completed seat.

**Not repaired here, deliberately.** Re-running `cs complete` on those molecules
with the fixed binary would restore their pointers, but it would also mutate
live fleet state outside this molecule's worktree and make an audited historical
record silently become green. Whether to do that is the operator's call, not
this change's.

**Left open, and named as open.** The roster lint classifies a witness failure
on a terminal molecule as `HISTORICAL … reported, not refused`, so it can never
redden anything. For a witness that — before this fix — could *only* be read
after the fact, that downgrade made the failure unactionable by construction,
which is the shape this whole lineage exists to refuse. The fix removes the
cause; it does not touch the downgrade. Whether `HISTORICAL` is the right class
for post-hoc-only witnesses is a live question and is not widened into here.

---

## C-005 — the register's own locators were pinned to a tree that no longer existed

**Corrects:** C-001 of this file, both of its locator sentences.

**What it said.**

> Measured on **the tree this correction ships against** … at
> `committee.rs:4196:68`, `:4197:60` and `:4198:28` **on this tree**; … and at
> `4186` on `1a719a3`, the parent.

**What is true.** Re-derived from the file blobs themselves rather than from a
log, at trees named by hash so the statement cannot drift again:

| tree | commit | the three `clippy::doc_markdown` (`base_url`) doc lines |
|---|---|---|
| `91b40780ba9fb75e746ad95f3ba3b99205bd2c6d` | `ada5532` | `committee.rs:4198`, `:4199`, `:4200` |
| tree of `1a719a3` | `1a719a3` | `committee.rs:4185`, `:4186`, `:4187` |

C-001's columns (68 / 60 / 28) are right and identify the same three sites; its
*lines* were two low, and on `1a719a3` it named `4186`, which is the middle site
rather than the first.

**How the drift happened, which is the part worth keeping.** The captured log
C-001 grepped (`task-20260729-d28d/clippy-all-targets.log`, mtime 02:15) was
taken on an intermediate working tree **nine minutes before** `6ac7571` was
authored (02:24), and two further lines landed in `committee.rs` in between. The
generator grepped the log faithfully. What it did not do is check that the log
described the tree it was about to ship — **the log's identity taken for the
tree's identity**, inside the very register written to close three instances of
one identity being taken for another.

**What survives, independently re-verified.** C-001's substantive claim is
sound: exactly 62 unique warning sites across exactly 7 crates, three of them in
`committee.rs`, the principal file `24ad753` edited. Nothing in C-002, C-003 or
C-004 depends on the drifted numbers.

**The rule this leaves behind, and it is stronger than "re-grep".** A locator is
pinned to a tree **named by hash**, never to the deictic "the tree this ships
against". That phrase cannot be true when it is written: the tree it points at
does not exist until the commit that contains the sentence is made, so the
author is always describing a different tree from the one the reader resolves
on. This correction obeys its own rule — every locator above names an immutable
ancestor, and **no locator here is stated against the tree this correction ships
on**, which edits `committee.rs` again and moves all three. Re-derive on any
tree with:

```text
git grep -n 'base_url' <tree> -- crates/cosmon-core/src/committee.rs | grep -v '`base_url`'
```

---

## C-006 — the suite count was reported in an accounting the baseline does not use

**Corrects:** commit message `6ac7571`, final paragraph; and the same figure in
`task-20260729-d28d/result.md`.

**What it said.**

> `cargo test --workspace` rc=0 at **338 suites** / 7349 passed / 0 failed

**What is true.** Measured by the round-2 driver on that exact tree
(`91b40780`): **325 suites** / 7349 passed / 0 failed. The `passed` figure
matches to the digit; only the suite figure differs, by exactly 13 — which is
the number of test output lines beginning with `test result_`
(`result_requires_jwt`, `result_404_when_molecule_absent`, …). There are 14
test names whose function identifier begins with `result_`; the fourteenth,
`result_status_hints_carry_the_exact_next_command`, prints with its module
prefix as `test hints::tests::result_status_hints_carry_the_exact_next_command`
and therefore escapes the loose prefix match. A count anchored on
`grep -c '^test result'` swallows the other 13 as if each were a suite.

**Why it is a finding and not a nit.** The suite count is the one number this
loop watches, and it watches it for **falling**: a suite that stops being built
never turns red, it goes silent. The baselines are stated in the other
accounting — 325 / 7342 at `1a719a3`, 324 / 7295 at `5198a39` — so a figure
inflated by 13 is not comparable to them, and could absorb the disappearance of
thirteen suites while still reading as a rise. The guard against silence is
disabled by the number that is supposed to be it. It is the defect class again,
in the commit that shipped this register to close it: a **label** (lines
matching a prefix) standing where a **property** (test binaries that ran) was
available.

**The rule this leaves behind.** Report the suite count in the accounting the
baseline uses, and **name the accounting in the same sentence as the number**,
so the next commit in the lineage cannot copy an ambiguous figure forward. The
accounting is:

```text
grep -cE '^test result: (ok|FAILED)\. [0-9]+ passed' test.out
```

Anything else — `grep -c '^test result'` above all — is a different measurement
and must not be labelled "suites".
