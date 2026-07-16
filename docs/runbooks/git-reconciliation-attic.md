# Runbook — Bundle & archive the pre-23-June cosmon attic (Zone-1 step 1d)

**Molecule:** `task-20260711-ee83` (child **C2** of `delib-20260711-4733`).
**Nature:** read-only on all repos; the only writes are (a) additive local git
config + annotated tags on `/srv/cosmon/cosmon`, (b) new files **outside** any
repo under `~/archive/`. **No history was rewritten and nothing was pushed.**
**Blocks:** C4 (the operator flip runbook cites the bundle paths + provenance
tags produced here).

For the full chain of custody — lineage roots, divergence, seals, integrity
boundary — read the companion **[`PROVENANCE.md`](PROVENANCE.md)** (mirrored
byte-identical at `~/archive/PROVENANCE.md`). This runbook records **what was
run** and **what was found**; PROVENANCE records **what it means**.

---

## What was run (exact commands, in order)

```bash
# 1. Protect the reflog from a background gc for the migration duration
git -C /srv/cosmon/cosmon config gc.reflogExpire never
git -C /srv/cosmon/cosmon config gc.reflogExpireUnreachable never

# 2. Archive directory (outside every repo)
mkdir -p ~/archive

# 3. Bundle Rank 1 — the living checkout (maximal: all three roots + attic)
git -C /srv/cosmon/cosmon bundle create ~/archive/cosmon-attic-living.bundle --all

# 4. Bundle Rank 2 — the clean bare archive (cosmon-private through 27 May)
git -C ~/scratch/cosmon-private-archive.git bundle create ~/archive/cosmon-attic-27may.bundle --all

# 5. Copy Rank 3 — the four filter-repo commit-maps, renamed per workspace
mkdir -p ~/archive/commit-maps
for w in migration genericize cc22 release; do
  cp ~/scratch/cosmon-$w/.git/filter-repo/commit-map ~/archive/commit-maps/commit-map.$w
done

# 6. Seal everything with BLAKE3
( cd ~/archive           && b3sum cosmon-attic-living.bundle cosmon-attic-27may.bundle )
( cd ~/archive/commit-maps && b3sum commit-map.* )

# 7. Plant immutable provenance anchors (annotated, LOCAL ONLY — not pushed)
git -C /srv/cosmon/cosmon tag -a provenance/lineage-A-root 11b8a15b4 -m "…"
git -C /srv/cosmon/cosmon tag -a provenance/lineage-B-root 7df0613b3 -m "…"
```

> **Deliberately NOT run** (Zone-2/3, out of C2 scope): no `git gc --prune`, no
> `remote rename`, no push of any ref or tag, no worktree mutation, no branch
> rewrite. The `gc.reflog*` config is a *duration-only* protection; unset it once
> the migration completes (`git config --unset gc.reflogExpire` ×2).

---

## Resulting archive tree

```
~/archive/
├── PROVENANCE.md                     # BLAKE3-sealed chain of custody
├── cosmon-attic-living.bundle        # 156,871,880 B · 92,052 obj · 356 refs
├── cosmon-attic-27may.bundle         # 168,133,280 B · 81,388 obj · 153 refs
└── commit-maps/
    ├── commit-map.migration          # 10,259 rows
    ├── commit-map.genericize         #  3,308 rows
    ├── commit-map.cc22               # 10,599 rows
    └── commit-map.release            # 10,694 rows
```

BLAKE3 seals for every file are tabulated in `PROVENANCE.md` §3.

---

## What was found (the three facts C4 must carry)

1. **The two lineages are the same house, twice-photographed.** Roots A
   `11b8a15b4` (parentless, `responses/` scrubbed) and B `7df0613b3` (`responses/`
   still present) have an **empty merge-base** and diverge by **exactly two
   files** — `responses/adversary.md`, `responses/godin.md`, both on the B side.
   A is the cleaner scrub ⇒ inferred run-order **B first, A second** (archival
   narrative only, not canonicity — see PROVENANCE §2).

2. **Lineage A / `main` is archived at perfect fidelity.** `git rev-list
   --objects 841bfbaa4` walks clean to root `11b8a15b4`; the to-be-published tree
   has zero missing objects.

3. **⚠️ Pre-existing gc wound on lineage B — 6 unreadable objects,
   unrecoverable.** B-root `7df0613b3` is a merge whose second parent
   `c61891dc…` was pruned *before any archive existed* (absent from the living
   checkout, the 27-May bare, all four scratch workspaces, and all four
   commit-maps). `git fsck --connectivity-only` names five more on the same arm.
   All 26 B-side refs transitively need `c61891dc`, so **a strict `git
   clone`/`fetch` of the living bundle aborts** — consumers must ingest with
   `git bundle unbundle` (object-level, tolerates the gap). This is the concrete
   proof of *why the delib's "bundle before gc" was time-critical*: a prior gc had
   already nicked B. **C3/C4 impact:** the four B-only branches sit above the
   broken arm; their recent commits cherry-pick, but deep-history diffs will hit
   the hole. Full detail + recovery recipe in PROVENANCE §4.

---

## Verification (re-runnable)

```bash
# Seals match
cd ~/archive && b3sum -c <<'EOF'
825d602eabc0ae4f0642b059246b9afdee28019c415452d986034ee6cb66b6b7  cosmon-attic-living.bundle
9d8e88dd9500169812cb782b8934ba9d14ad7647c47c8c847ec02a33f57d10ae  cosmon-attic-27may.bundle
EOF

# Three roots really are inside the living bundle
git init --bare /tmp/verify.git
git -C /tmp/verify.git bundle unbundle ~/archive/cosmon-attic-living.bundle
for r in 11b8a15b4 7df0613b3 7e1b361; do git -C /tmp/verify.git cat-file -t "$r"; done  # commit ×3

# PROVENANCE.md self-seal
awk '{print} /BLAKE3 SEAL BELOW/{exit}' ~/archive/PROVENANCE.md | b3sum
#   → fc6376037d49e469cef90d5e9b267b855672900b1d5fb780d5cf13f2ea3393c4

# Provenance tags (local)
git -C /srv/cosmon/cosmon tag -l 'provenance/*' -n1
```

---

## Provenance anchors (annotated tags, local — not pushed)

| Tag | Tag object | → commit |
|---|---|---|
| `provenance/lineage-A-root` | `0f0802e9b0601ddfca257c329008ec83a3fe2540` | `11b8a15b446e8b83a0544d20f40a060f47fe7220` |
| `provenance/lineage-B-root` | `b41f58d539c7729d4b9b47a0c6944aaebb566241` | `7df0613b3dfae0d84ab036bca5c1d8a4d5a20b56` |

These are additive local refs on `/srv/cosmon/cosmon`. Whether to push them (to
`cosmon-private` as frozen provenance, per delib Q6) is an **operator** decision
in C4 — this worker did not push. GPG signing was skipped (no signing key
configured on this machine); the BLAKE3 seals in `PROVENANCE.md` carry the
integrity trace in the interim.
