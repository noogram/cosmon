# Git reconciliation — Zone-2a salvage gate (SALVAGE GATE G1)

**Molecule:** `task-20260711-2404` · **Blocks:** C4 (flip runbook/ADR) ·
**Upstream:** `delib-20260711-4733` §C3 · **Status of gate:**
`RESOLVED — pending operator write-off approval` (see §5).

> **The rule.** *Silence ≠ salvage.* The gate closes only when the B-only
> branch enumeration returns **empty**. This document records an explicit
> **verdict per branch** so the emptying is a decision on record, not an
> accident of a re-clone. Cherry-picks (if any) are *work on lineage A* and
> must merge **before** fleet quiescence is measured; write-offs are the
> operator's call (propose, do not decide alone).

---

## 0. What A and B are

The repository carries **two disjoint histories** (no common ancestor):

| Lineage | Root commit | Tip | Nature |
|---------|-------------|-----|--------|
| **A** (main) | `11b8a15b4` | `841bfbaa4` | The reconciled/live timeline. Full internal tree. |
| **B** | `7df0613b3` | (four local branches) | An orphaned June 23–24 2026 "publication-prep" epoch. Its **pushed** `origin/*` refs are already ⊆ A (torvalds). |

`git merge-base 841bfbaa4 <any-B-branch>` → **empty** (disjoint roots). The four
branches below are **local only** (`track=none`), descend from B-root
`7df0613b3`, and are **not** ancestors of A-tip `841bfbaa4`. A re-clone or a
prune during the flip would silently kill them — hence this gate.

### The four B-only branches and their containment

```
feat/mission-20260623-52b6  (e7383d74c)   ⊂
  feat/task-20260623-abda   (eac753718)   ⊂
    ├── feat/task-20260624-b654  (17f7e0fe2)          ← +1 commit after 28ac60fbe
    └── feat/bake-v2.5-version-bump (b1783d74c)       ← +10 commits after 28ac60fbe
        (b654 and bake diverge at 28ac60fbe "Merge feat/task-20260623-5eb9")
```

`feat/bake-v2.5-version-bump` is the superset tip (44 commits vs A: 26 non-merge).
`feat/task-20260624-b654` adds exactly one commit not in bake
(`17f7e0fe2`, a doc-label sanitization). Salvage scope therefore =
**bake ∪ {17f7e0fe2}**.

---

## 1. Method — how each verdict was reached

For the whole salvage scope we computed the set of files **present in a B tip
tree but absent from A** (the only place unique content can hide), then
inspected each:

```bash
A=841bfbaa4; BAKE=feat/bake-v2.5-version-bump; B654=feat/task-20260624-b654
comm -23 \
  <(git ls-tree -r --name-only $BAKE $B654 | sort -u) \
  <(git ls-tree -r --name-only $A       | sort)
```

That yields **11 files**. Every headline deliverable of the epoch
(ADR-132, ADR-133, the `cosmon-metrics-fingerprint` paper, the rpp-adapter
**JWKS-by-HTTP-fetch** at version **2.5.0**) was checked and is **already in
A, in A's newer/fuller form**:

| Deliverable | In A? | Evidence |
|-------------|-------|----------|
| ADR-132 kernel + plugin catalog | ✅ identical | `docs/adr/132-kernel-plugin-catalog-ecosystem.md` present both sides |
| ADR-133 one-repo artifact-map membrane | ✅ identical | `docs/adr/133-one-repo-artifact-map-membrane.md` present both sides |
| `cosmon-metrics-fingerprint` paper (16 files) | ✅ identical set | internal metrics-study paper files present both sides |
| rpp-adapter **JWKS HTTP-fetch** | ✅ **A is ahead** | `jwks_uri` in **8** A source files vs **3** in B; both at `version = "2.5.0"` |

The 11 net-new-in-B files break down as: 7 stale typo-duplicates, 2 gitignored
scratch, 1 obsolete test, 1 superseded evidence — detailed per branch below.

---

## 2. Per-branch verdicts

### 2.1 `feat/mission-20260623-52b6` → tip `e7383d74c`

**Verdict: WRITE-OFF.** This is the innermost ancestor of the other three
(19 commits). It introduces **zero** files absent from A; all of its content is
a strict subset of `feat/task-20260623-abda`, which is itself written off below.
Nothing unique to salvage.

- Recoverable pre-image tag: `salvage/mission-20260623-52b6-e7383d74c`

### 2.2 `feat/task-20260623-abda` → tip `eac753718`

**Verdict: WRITE-OFF.** Adds the `metrics-study` empirical-data + paper-draft
commits (`67138c733`, `726c4f950`, `2dee34456`, …). **All of these files exist
verbatim in A** under an internal paper-draft directory. No net-new file. Nothing to
salvage.

- Recoverable pre-image tag: `salvage/task-20260623-abda-eac753718`

### 2.3 `feat/task-20260624-b654` → tip `17f7e0fe2`

**Verdict: WRITE-OFF.** Extends abda. Two categories of net-new-in-B content,
both non-salvageable:

1. **`evidence/traces-phase2/traces-review-phase2.md`** (12 854 B, dated
   2026-04-16) — a Phase-2 strace security review. A **retains the raw
   `evidence/traces-phase2/*.trace` files** and carries a **later**
   `evidence/traces-phase2b/traces-review-phase2b.md`. This markdown is
   **superseded historical evidence**; the raw traces it reviews are preserved
   in A. *(This is the single "judgment-call" item — see §4.)*
2. `17f7e0fe2` itself only edits three already-in-A metrics-paper files to
   sanitize a confidential label; A already carries sanitized copies.

- Recoverable pre-image tag: `salvage/task-20260624-b654-17f7e0fe2`

### 2.4 `feat/bake-v2.5-version-bump` → tip `b1783d74c`

**Verdict: WRITE-OFF.** The superset tip; headline = "bake JWKS HTTP-fetch"
+ version bump 2.2.0→2.5.0. **Both are already in A** (A's rpp-adapter is at
`2.5.0` and carries `jwks_uri` HTTP-fetch across *more* files than B). Its
net-new-in-B files are all non-salvageable:

| File(s) | Bytes | Why write-off |
|---------|-------|---------------|
| 7× `docs/adr/0{76,86,87,89,90,91}-**almanac**-*.md` + an internal almanac-internalisation chronicle | ~135 KB | **Stale typo-duplicates.** A carries the identical documents under the corrected `**almanac**` spelling (typo fixed 2026-06-27); the only diffs are the `almanac→almanac` corrections (48 lines on ADR-076, all typo). A is the corrected descendant. |
| `responses/adversary.md`, `responses/godin.md` | ~41 KB | **Gitignored scratch.** Panel responses from `delib-20260616-0f4f`. A **deliberately untracked** repo-root private panel responses (see A `.gitignore` §"Private molecule declaration/artifact directory" — *"Tracked copies were old delib/idea artifacts (private panel responses …); only tracking is removed"*). Salvaging them would re-violate A's own convention; the live artifacts live in `.cosmon/state/`. |
| `tests/scenarios/collapse-cascades.toml` | 784 B | **Obsolete test — would break A.** Asserts *"collapse of a blocker **cascades** to B (B→Collapsed, never runs)"*. A **deliberately reversed** this: `tests/scenarios/collapse-releases-successors.toml` (task-20260706-4d1e) asserts collapse **releases** successors. Cherry-picking would add a test contradicting current code. |

- Recoverable pre-image tag: `salvage/bake-v2.5-version-bump-b1783d74c`

---

## 3. Summary table

| Branch | Tip SHA | Unique content vs A | Verdict | Pre-image tag |
|--------|---------|---------------------|---------|---------------|
| `feat/mission-20260623-52b6` | `e7383d74c` | none (⊂ abda) | **WRITE-OFF** | `salvage/mission-20260623-52b6-e7383d74c` |
| `feat/task-20260623-abda` | `eac753718` | none (metrics ∈ A) | **WRITE-OFF** | `salvage/task-20260623-abda-eac753718` |
| `feat/task-20260624-b654` | `17f7e0fe2` | 1 superseded evidence md | **WRITE-OFF** | `salvage/task-20260624-b654-17f7e0fe2` |
| `feat/bake-v2.5-version-bump` | `b1783d74c` | 7 typo-dups + 2 scratch + 1 obsolete test | **WRITE-OFF** | `salvage/bake-v2.5-version-bump-b1783d74c` |

**Cherry-picks onto A: none.** Every substantive deliverable is already in A,
in A's newer/fuller form. No work-on-A was produced, so nothing needs to merge
before quiescence is measured (the G1+G2 forbidden bundling does not arise).

---

## 4. The one judgment-call — flagged, not decided

`evidence/traces-phase2/traces-review-phase2.md` (§2.3) is the only net-new-in-B
file that is neither stale, scratch, nor obsolete — it is a genuine but
**superseded** security-review artifact. The recommendation is **WRITE-OFF**
(A keeps the raw traces + the later phase2b review). If the operator wants the
narrative review preserved, the one-line salvage is:

```bash
git show feat/task-20260624-b654:evidence/traces-phase2/traces-review-phase2.md \
  > evidence/traces-phase2/traces-review-phase2.md   # run on main (A)
git add evidence/traces-phase2/traces-review-phase2.md && git commit -m \
  "docs(evidence): salvage phase-2 strace security review from B lineage"
```

This is content-only (no B lineage), conflict-free, and does not re-activate any
worktree.

---

## 5. Closing the gate — operator write-off gesture

The salvage analysis is complete and every verdict is WRITE-OFF, but **executing
a write-off = deleting the branch ref**, which is a destructive operator gesture
(worker/human boundary; delib §C3 *"proposer, ne pas trancher seul les
write-offs"*). The gate is therefore **RESOLVED-PENDING-APPROVAL**, not yet
empty, by design.

**Recoverability is already in place** — the four `salvage/*` tags (§3) preserve
every tip SHA. The branch-delete below is fully reversible from those tags for
the life of the private provenance repo, and the tags live in `refs/tags` so
they do **not** re-trigger the gate (which scans `refs/heads` only).

### Operator close command (run after approving the four write-offs)

```bash
cd /srv/cosmon/cosmon

# 1. (safety) confirm the salvage tags exist — recovery net
git tag -l 'salvage/*'   # must list the four tips

# 2. WRITE-OFF: delete the four B-only local branch refs
git branch -D feat/mission-20260623-52b6 \
              feat/task-20260623-abda \
              feat/task-20260624-b654 \
              feat/bake-v2.5-version-bump

# 3. PROVE THE GATE IS EMPTY (must print nothing)
for b in $(git for-each-ref --format='%(refname:short)' refs/heads); do
  git merge-base --is-ancestor 7df0613b3 "$b" 2>/dev/null \
    && ! git merge-base --is-ancestor "$b" 841bfbaa4 2>/dev/null \
    && echo "B-ONLY, NOT in A: $b"
done
```

When step 3 prints nothing, **SALVAGE GATE G1 is empty** and C4's flip runbook
may state so. To recover any written-off branch later:
`git branch <name> salvage/<name>-<sha>`.

### Enumeration state at the time this runbook was written

```
B-ONLY, NOT in A: feat/bake-v2.5-version-bump
B-ONLY, NOT in A: feat/mission-20260623-52b6
B-ONLY, NOT in A: feat/task-20260623-abda
B-ONLY, NOT in A: feat/task-20260624-b654
```

Four branches — all analyzed, all verdict WRITE-OFF, all pre-imaged under
`salvage/*` tags, awaiting the operator's one-gesture deletion in §5.

---

## Verification record (step 2)

Change is **docs-only** (`docs/runbooks/git-reconciliation-salvage.md` — the sole
tracked file added; four non-destructive `salvage/*` tags created).

| Gate | Result |
|------|--------|
| `cargo check --workspace` | ✅ PASS (exit 0) |
| `cargo clippy --workspace -- -D warnings` | ✅ PASS (no warnings) |
| `cargo fmt --all -- --check` | ✅ PASS (no diff) |
| `cargo test --workspace` | ✅ every suite `0 failed` (cs binary suite **1258/1258**); the aggregate workspace run exceeds the 13-min timeout cap because of the pre-existing heavy integration suites (334s + 56s + 50s + …), not a hang (test-spawned `cs` procs were CPU-pegged) and not a regression — outcome is identical to main's baseline for a docs-only change. |

**Salvage-gate final state:** four B-only branches remain (all verdict WRITE-OFF,
all pre-imaged under `salvage/*` tags). The gate is closed by the operator's
one-gesture branch deletion documented in §5 — a destructive write-off reserved
for the operator per delib §C3. No cherry-picks were needed (zero unique content
in B).
