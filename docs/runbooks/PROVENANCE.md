# PROVENANCE — cosmon git reconciliation attic

**Chain of custody for the pre-23-June cosmon history, bundled before any
`git gc --prune` could sweep it.** Produced by molecule `task-20260711-ee83`
(child C2 of `delib-20260711-4733`). Read this file to understand what the two
`~/archive/*.bundle` files contain, how the two "lineages" relate, and where the
history is already holed.

> This document is a **trace, not a lock** (cosmon briefing-seal doctrine,
> `docs/architectural-invariants.md` §8b). The BLAKE3 seal at the foot catches a
> lazy post-hoc edit; it is not PKI. Anyone with filesystem access can rewrite
> both the file and its seal.

Canonical copy lives beside the bundles at `~/archive/PROVENANCE.md`; the tracked
copy is `docs/runbooks/PROVENANCE.md` on lineage A. The two are byte-identical.

---

## 1. The picture in one breath

One real house, two photographs of it.

- **Lineage A** — the living `/srv/cosmon/cosmon`. Its root `11b8a15b4` is a clean,
  **parentless** commit produced by `git filter-repo` (the `cc22`/`release`
  address-blur run). A is the fully-scrubbed public candidate: it **removed**
  `responses/` — a folder B still leaks.
- **Lineage B** — the `cosmon-private` remote (`origin/*` in the living checkout).
  Its representative root `7df0613b3` is the *same logical merge* as A's root
  (identical subject `Merge branch 'feat/task-20260623-80f9'`, identical author
  timestamp `1782214464`), but from the **earlier, less-aggressive** filter-repo
  run (`migration`/`genericize`). B still **carries** `responses/adversary.md`
  and `responses/godin.md`.

Same bricks, different brick-numbers — because `filter-repo` re-numbered every
object on each of two runs. That is why git's `ahead/behind` ruler reads
gibberish between A and B, and why their **merge-base is empty**.

---

## 2. The two lineage roots (recorded facts)

| | Lineage A (public candidate) | Lineage B (cosmon-private photo) |
|---|---|---|
| Root commit | `11b8a15b446e8b83a0544d20f40a060f47fe7220` | `7df0613b3dfae0d84ab036bca5c1d8a4d5a20b56` |
| Root tree | `3bd7857bb3d0c88ca5ab260990036b5f95c050c7` | `ff7576574a3a1f561d745c557204a0b7d7a216df` |
| Parents | **none** (parentless — filter-repo truncated) | `77f33775…` (present) **+** `c61891dc…` (**PRUNED**) |
| Subject / date | `Merge branch 'feat/task-20260623-80f9'` / `1782214464 +0200` | *(identical)* |
| `responses/` | **absent** (0 entries) — scrubbed | **present** (2 entries) — leaked |
| filter-repo run | `cc22` / `release` (later, cleaner) | `migration` / `genericize` (earlier) |
| Annotated tag | `provenance/lineage-A-root` → `0f0802e9b0601ddfca257c329008ec83a3fe2540` | `provenance/lineage-B-root` → `b41f58d539c7729d4b9b47a0c6944aaebb566241` |

- **Merge-base(A, B) = ∅** (empty). Verified: `git merge-base 11b8a15b4 7df0613b3`
  prints nothing.
- **Divergence = exactly 2 files.** `git diff --name-status 11b8a15b4 7df0613b3`
  reports only `responses/adversary.md` and `responses/godin.md`, both **added on
  the B side** (blobs `610fd5f5…` and `197fad3b…`). Everything else is identical.
- **Third root — the pre-scrub original.** `7e1b3615658069e4ae60db0d8d9a863792ba0c03`
  (`feat: bootstrap wiki-genetic-algorithms…`, 2026-04-13) is the attic's real
  bottom: ~10k un-rebased commits root here, from before any address-blur.

### Run-order — inferred, **archival narrative only, not canonicity**

**B first, A second.** Direct wall-clock metadata for the two filter-repo runs is
gone; the ordering is inferred from the *recipe lineage*: A purges `responses/`
that B still carries, so A is the later, more-refined scrub (0 vs 2 `responses/`
entries — recorded fact). A GitHub PushEvent probe (delib Q-op-3) would upgrade
this from inference to wall-clock fact; it is an archival nicety and gates
nothing. **Ordering does not confer canonicity** — the canonical tree is A by the
operator's publish decision, independent of which scrub ran first.

---

## 3. The bundles (what was preserved, and its b3 seal)

Created with `git bundle create … --all` on 2026-07-11, **before any `gc --prune`**.
`gc.reflogExpire` and `gc.reflogExpireUnreachable` were set to `never` on
`/srv/cosmon/cosmon` for the migration duration first, so a background gc cannot
race the archive.

| Bundle | Bytes | Objects | Refs | BLAKE3 (b3sum) |
|---|---|---|---|---|
| `~/archive/cosmon-attic-living.bundle` | 156 871 880 | 92 052 | 356 | `825d602eabc0ae4f0642b059246b9afdee28019c415452d986034ee6cb66b6b7` |
| `~/archive/cosmon-attic-27may.bundle` | 168 133 280 | 81 388 | 153 | `9d8e88dd9500169812cb782b8934ba9d14ad7647c47c8c847ec02a33f57d10ae` |

- **`cosmon-attic-living.bundle`** = `--all` of the living `/srv/cosmon/cosmon`
  (Rank 1). Contains **all three roots** — A `11b8a15b4`, B `7df0613b3`, pre-scrub
  `7e1b361` — plus the whole attic and every current worktree HEAD. This is the
  **maximal** snapshot: every object that still exists in the living checkout.
- **`cosmon-attic-27may.bundle`** = `--all` of the bare `~/scratch/cosmon-private-archive.git`
  (Rank 2). A clean snapshot of cosmon-private **through 27 May** (tip
  `8216ce24982df1d988ac9afe40578e5009395940`). It **predates** the 23-June A/B
  divergence, so it holds none of `11b8a15b4`, `7df0613b3`, `c61891dc`, or
  `77f33775` — it is an earlier, independently-clean witness, not a backfill for
  the June hole below.

### commit-maps (Rank 3 — OLD→NEW dictionaries, objects gc'd but maps intact)

Copied verbatim from each scratch workspace's `.git/filter-repo/commit-map`
(format: `old<40>  new<40>`, one pair per line; deletions map to `0000…`).

| File (`~/archive/commit-maps/`) | Source workspace | Rows | BLAKE3 |
|---|---|---|---|
| `commit-map.migration` | `~/scratch/cosmon-migration` | 10 259 | `a8291c2f14a47d0c0e18b20e9cad19b5e990b62ca18f4eb81bbc9f8c1d2b305f` |
| `commit-map.genericize` | `~/scratch/cosmon-genericize` | 3 308 | `a35af18e42737c5b7043acf6da3259480b4dc29114ca01288a8800a45fec965a` |
| `commit-map.cc22` | `~/scratch/cosmon-cc22` | 10 599 | `631a743e1d0128cd5ba018e6e620754721d9dc46c0f32ebe0b973e7d97902372` |
| `commit-map.release` | `~/scratch/cosmon-release` | 10 694 | `e0350f9c223ae4be2062fb2b3a0935c3c52b24f7ee1cd78ed2aad428402c42ef` |

---

## 4. Integrity boundary — READ BEFORE RELYING ON THE ARCHIVE

The living checkout **already had a pre-existing gc wound** when it was bundled.
This is not archive corruption; the bundle faithfully preserved every object that
still existed. The wound is **confined to lineage B** and is **unrecoverable**
(the objects are absent from the living checkout, the 27-May bare archive, all
four scratch workspaces, and all four commit-maps).

- **Lineage A / `main` is FULLY CONNECTED.** `git rev-list --objects 841bfbaa4`
  (the current `main` tip, `docs(lore): chronicle the frozen-conflation…`) walks
  clean to root `11b8a15b4` with zero missing objects. **The to-be-published tree
  is archived at perfect fidelity.**
- **Lineage B has 6 unreadable objects.** B-root `7df0613b3` is a *merge*; its
  second parent `c61891dc5410319a867d8619b8941ac78ee11ec4` was pruned. `git fsck
  --connectivity-only` reports 5 further pruned objects on the same arm:
  `4db55949…`, `64b96ccf…`, `7890873c…`, `bb5ae0f8…`, `cef0deb4…`. All 26 B-side
  refs (`origin/main`, the four B-only `feat/*` local branches, the `origin/feat/*`
  fleet branches) transitively require `c61891dc` and therefore cannot be walked
  past the merge on that arm. The **other** parent `77f33775…` is present, so B's
  history survives through that arm.

### Consequence for consumers — INGEST VIA `unbundle`, NOT `clone`

Because of the broken B-arm, a strict `git clone`/`git fetch` from
`cosmon-attic-living.bundle` **aborts** with *"did not send all necessary
objects"*. Use the object-level path, which tolerates the gap:

```bash
git init --bare recovered.git
git -C recovered.git bundle unbundle ~/archive/cosmon-attic-living.bundle
# 92,052 objects land; A-root, A/main-tip, and B-root 7df0613b3 all readable.
# Lineage A checks out cleanly; B tips are present but not walkable past the
# c61891dc merge-arm.
```

**Downstream note for C3 (salvage) and C4 (flip runbook):** the four B-only
branches (`feat/bake-v2.5-version-bump`, `feat/mission-20260623-52b6`,
`feat/task-20260623-abda`, `feat/task-20260624-b654`) sit **above** the broken
arm — their tip commits and recent trees exist, so cherry-picking recent commits
onto A remains possible, but any operation that walks their deep B ancestry will
hit the `c61891dc` hole. Verify per-branch before relying on full-history diffs.

---

## 5. Verification recipe

```bash
# Bundle seals
cd ~/archive && b3sum cosmon-attic-living.bundle cosmon-attic-27may.bundle
cd ~/archive/commit-maps && b3sum commit-map.*
# roots present in the living bundle (unbundle, then read the three roots)
git init --bare /tmp/v.git && git -C /tmp/v.git bundle unbundle ~/archive/cosmon-attic-living.bundle
for r in 11b8a15b4 7df0613b3 7e1b361; do git -C /tmp/v.git cat-file -t "$r"; done   # all: commit
# lineage divergence
git -C /srv/cosmon/cosmon diff --name-status 11b8a15b4 7df0613b3   # only responses/{adversary,godin}.md
git -C /srv/cosmon/cosmon merge-base 11b8a15b4 7df0613b3           # empty
```

> **Note on `git bundle verify`.** For an `--all` (complete-history) bundle,
> `git bundle verify` prints *"The bundle records a complete history"* and lists
> only the **ref tips** — it does **not** enumerate root commits (roots are only
> printed as *prerequisites* for thin/incremental bundles, of which these have
> none). Root presence is therefore proven by the `unbundle`+`cat-file` recipe
> above, not by `verify` output. The delib's phrasing ("must list roots …")
> assumed an incremental bundle; the correct check is the one shown here.

---

<!-- ===== BLAKE3 SEAL BELOW — every byte ABOVE and INCLUDING this line is sealed ===== -->
**BLAKE3 seal (this file, bytes through the marker line above):** `fc6376037d49e469cef90d5e9b267b855672900b1d5fb780d5cf13f2ea3393c4`

Verify: `awk '{print} /BLAKE3 SEAL BELOW/{exit}' PROVENANCE.md | b3sum`
