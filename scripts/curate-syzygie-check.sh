#!/usr/bin/env bash
# curate-syzygie-check.sh — v1 sanity check before flipping the curate
# patrol's allowlist from cosmon-only to {cosmon, mailroom, accord,
# knowledge, showroom}.
#
# Parent: delib-20260521-c3cd (cross-galaxy drain patrol architecture v0).
# Child:  task-20260521-62ab (this script, the v1 template, the gate audit).
#
# Why this exists. Crossing the patrol into peer galaxies means the
# classifier can collapse a molecule that another galaxy's chronicle or
# ADR cites — silently breaking the syzygie citation graph. The matrix
# row `syzygie_cited == true → Surface (never Collapse)` prevents it,
# but only if the input set of cited molecules is correctly tagged.
#
# What this script does, in order:
#   (1) Enumerate pending molecule IDs in each scope galaxy.
#   (2) For each pending ID, grep every OTHER galaxy's
#       docs/lore/CHRONICLES.md + docs/adr/ for a citation.
#   (3) Print/tag each cross-galaxy-cited pending with
#       `temp:syzygie-cited` so the classifier's syzygie row fires.
#   (4) Sweep every cited ID found in any chronicle/ADR and check that
#       a molecule with that ID still exists somewhere. Report orphans.
#
# Idempotent. `--dry-run` (default) only prints; `--apply` writes tags.
# Re-running with no changes prints zeros and tags nothing new.
#
# Exit codes:
#   0  clean — zero orphans, scope tagged (or dry-run report only)
#   1  orphan citations found — operator must inspect before v1 flip
#   2  scope galaxy missing .cosmon — config error
#   64 bad CLI args

set -euo pipefail

# ---------------------------------------------------------------------------
# defaults
# ---------------------------------------------------------------------------

GALAXIES_ROOT="${HOME}/galaxies"
SCOPE=()                      # filled below or via --scope
DRY_RUN=1                     # default: report-only
OUT_PATH=""                   # default: stdout
SYZYGIE_TAG="temp:syzygie-cited"

# Pattern lifted from scripts/curate-syzygie-cache.sh — keep in sync.
# Cosmon short ID: <kind>-YYYYMMDD-<4 hex>.
PATTERN='\b(task|idea|decision|deliberation|delib|issue|signal|spark|mol|mission|patrol|vg|retro|sess|drift)-[0-9]{8}-[0-9a-f]{4}\b'

usage() {
  cat <<EOF
curate-syzygie-check.sh — pre-v1 syzygie audit.

Usage:
  $(basename "$0") [--scope g1,g2,...] [--galaxies-root DIR]
                   [--apply] [--out FILE] [--tag NAME]

  --scope         Comma-separated galaxy names (default:
                  cosmon,mailroom,accord,knowledge,showroom).
  --galaxies-root Override ~/galaxies for testing.
  --apply         Actually tag cited pending molecules (default: dry-run).
  --out FILE      Write report to FILE (default: stdout).
  --tag NAME      Tag name to apply (default: temp:syzygie-cited).
  -h, --help      Show this help.

Exit codes:
  0  clean      zero orphan citations
  1  orphans    citations point to non-existent molecules
  2  config     scope galaxy missing .cosmon
  64 cli        bad CLI args
EOF
}

# ---------------------------------------------------------------------------
# arg parse
# ---------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
  case "$1" in
    --scope)         IFS=',' read -ra SCOPE <<< "$2"; shift 2 ;;
    --galaxies-root) GALAXIES_ROOT="$2"; shift 2 ;;
    --apply)         DRY_RUN=0; shift ;;
    --out)           OUT_PATH="$2"; shift 2 ;;
    --tag)           SYZYGIE_TAG="$2"; shift 2 ;;
    -h|--help)       usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage >&2; exit 64 ;;
  esac
done

if [[ ${#SCOPE[@]} -eq 0 ]]; then
  SCOPE=(cosmon mailroom accord knowledge showroom)
fi

if [[ ! -d "$GALAXIES_ROOT" ]]; then
  echo "galaxies root not found: $GALAXIES_ROOT" >&2
  exit 2
fi

# Validate scope: each galaxy must have a .cosmon dir.
for G in "${SCOPE[@]}"; do
  if [[ ! -d "$GALAXIES_ROOT/$G/.cosmon" ]]; then
    echo "scope galaxy missing .cosmon: $GALAXIES_ROOT/$G" >&2
    exit 2
  fi
done

# ---------------------------------------------------------------------------
# python-driven core — TOML/JSON walking is fragile in pure bash and the
# galaxy molecule counts are in the thousands. Python keeps it correct.
# ---------------------------------------------------------------------------

/usr/bin/env python3 - "$GALAXIES_ROOT" "$DRY_RUN" "$OUT_PATH" "$SYZYGIE_TAG" "$PATTERN" "${SCOPE[@]}" <<'PY'
import json
import os
import re
import subprocess
import sys
from pathlib import Path

galaxies_root = Path(sys.argv[1]).expanduser()
dry_run = sys.argv[2] == "1"
out_path = sys.argv[3]
tag_name = sys.argv[4]
pattern = re.compile(sys.argv[5])
scope = sys.argv[6:]

def emit(line=""):
    if out_path:
        with open(out_path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    else:
        print(line)

# Truncate output file if writing to one.
if out_path:
    open(out_path, "w").close()

# -----------------------------------------------------------------
# Step 1: enumerate molecule IDs per galaxy (pending + universe).
# -----------------------------------------------------------------
# `pending_by_galaxy[g]` = set of pending mol IDs in galaxy g.
# `universe_by_galaxy[g]` = set of ALL mol IDs in galaxy g (any status).
# `universe` = union of all IDs across the entire workspace — used for
#              orphan detection (an ID is an orphan if it appears in a
#              chronicle/ADR but does not exist anywhere).
pending_by_galaxy: dict[str, set[str]] = {}
universe_by_galaxy: dict[str, set[str]] = {}
universe: set[str] = set()

# Walk EVERY galaxy under the root for the universe set, not just scope:
# a citation in cosmon may point to a knowledge molecule even if knowledge
# isn't yet in the patrol scope. Orphan detection must see the full set.
#
# Two flavours of "exists" feed the universe:
#  (i) cosmon-tracked: .cosmon/state/fleets/*/molecules/<id>/state.json
# (ii) doc-only: a doc-only deliberations directory — pre-cosmon-managed deliberations
#      that were never wired into a state store (common in showroom,
#      lumen, sociosynth, etc.). These ARE real artefacts; citing them is
#      not an orphan. Without (ii) the orphan list balloons with
#      non-actionable noise and the operator stops trusting it.
DELIB_ID_RE = re.compile(r'^(?:delib|deliberation)-\d{8}-[0-9a-f]{4}$')
for gdir in sorted(galaxies_root.iterdir()):
    if not gdir.is_dir():
        continue
    g = gdir.name
    pending: set[str] = set()
    all_ids: set[str] = set()
    fleets = gdir / ".cosmon" / "state" / "fleets"
    if fleets.is_dir():
        for state_file in fleets.glob("*/molecules/*/state.json"):
            mol_id = state_file.parent.name
            all_ids.add(mol_id)
            try:
                with state_file.open() as f:
                    data = json.load(f)
                status = data.get("status", "")
                if isinstance(status, dict):
                    status = next(iter(status))
                if status == "pending":
                    pending.add(mol_id)
            except (OSError, json.JSONDecodeError):
                # Defensive: treat unreadable as part of universe (it
                # exists on disk) but not pending (we can't promise
                # it's actionable).
                pass
    delib_dir = gdir / "docs" / "deliberations"
    if delib_dir.is_dir():
        for child in delib_dir.iterdir():
            if child.is_dir() and DELIB_ID_RE.match(child.name):
                all_ids.add(child.name)
    pending_by_galaxy[g] = pending
    universe_by_galaxy[g] = all_ids
    universe |= all_ids

# -----------------------------------------------------------------
# Step 2: collect citations per galaxy.
# `citations[g]` = list of (cited_id, file_path, lineno, context).
# We walk EVERY galaxy (not just scope) because v1 onboarding may add
# a galaxy whose chronicles already cite molecules from scope galaxies.
# -----------------------------------------------------------------
citations: dict[str, list[tuple[str, str, int, str]]] = {}
for gdir in sorted(galaxies_root.iterdir()):
    if not gdir.is_dir():
        continue
    g = gdir.name
    sources: list[Path] = []
    chron = gdir / "docs" / "lore" / "CHRONICLES.md"
    if chron.is_file():
        sources.append(chron)
    adr_dir = gdir / "docs" / "adr"
    if adr_dir.is_dir():
        for adr_file in adr_dir.rglob("*.md"):
            sources.append(adr_file)
    bucket: list[tuple[str, str, int, str]] = []
    for src in sources:
        try:
            with src.open(encoding="utf-8", errors="replace") as f:
                for ln, line in enumerate(f, start=1):
                    for m in pattern.finditer(line):
                        bucket.append((m.group(0), str(src), ln, line.rstrip()))
        except OSError:
            continue
    citations[g] = bucket

# -----------------------------------------------------------------
# Step 3: cross-galaxy citation graph — which scope-galaxy PENDINGS
# are cited from ANOTHER galaxy's chronicles/ADRs?
# -----------------------------------------------------------------
# Map mol_id → galaxy of origin (best effort: a mol ID is unique only
# within its galaxy, but the IDs are random 4-hex suffixes so collision
# across galaxies is rare. We pick the first match in scope order, then
# any galaxy.)
def find_origin(mol_id: str) -> str | None:
    for g in scope:
        if mol_id in universe_by_galaxy.get(g, set()):
            return g
    for g, ids in universe_by_galaxy.items():
        if mol_id in ids:
            return g
    return None

# `cited_pending[g]` = pending molecules in scope galaxy g cited by ≥1
#   other galaxy. These get the syzygie tag.
cited_pending: dict[str, dict[str, list[tuple[str, int]]]] = {g: {} for g in scope}

for g_citing, rows in citations.items():
    for cited_id, fpath, lineno, _ctx in rows:
        origin = find_origin(cited_id)
        if origin is None:
            continue
        if origin == g_citing:
            # Self-citation (cosmon ADR mentions a cosmon mol) — not
            # cross-galaxy. The single-galaxy patrol already protects
            # these via the godel firebreak (curate-* formulas) and the
            # in-scope tag set. v1's syzygie row only fires for
            # CROSS-galaxy citations.
            continue
        if origin not in scope:
            # Cited molecule lives in a non-scope galaxy (e.g. arly).
            # Can't tag it (the patrol won't visit), and it's not a
            # v1 risk because the patrol won't collapse it.
            continue
        bucket = cited_pending[origin].setdefault(cited_id, [])
        bucket.append((g_citing, lineno))

# -----------------------------------------------------------------
# Step 4: orphan detection — every cited ID, anywhere, that does not
# exist in the universe is an orphan citation. Operator must fix
# (either restore the molecule or amend the chronicle) before v1 flip.
# -----------------------------------------------------------------
orphans: list[tuple[str, str, int, str]] = []
for g_citing, rows in citations.items():
    for cited_id, fpath, lineno, ctx in rows:
        if cited_id not in universe:
            orphans.append((cited_id, fpath, lineno, ctx))

# -----------------------------------------------------------------
# Step 5: tag application (optional).
# -----------------------------------------------------------------
tag_results: list[tuple[str, str, str]] = []  # (galaxy, mol_id, result)
if not dry_run:
    for g, ids in cited_pending.items():
        gdir = galaxies_root / g
        for mol_id in sorted(ids.keys()):
            # Use the `cs` CLI with cwd set to the galaxy root (walk-up
            # discovery). Idempotent: `cs tag --add` is a set-insert,
            # not a list-append.
            try:
                proc = subprocess.run(
                    ["cs", "tag", mol_id, "--add", tag_name],
                    cwd=str(gdir),
                    capture_output=True,
                    text=True,
                    timeout=15,
                )
                if proc.returncode == 0:
                    tag_results.append((g, mol_id, "OK"))
                else:
                    err = (proc.stderr or proc.stdout or "").strip().splitlines()
                    msg = err[-1] if err else f"rc={proc.returncode}"
                    tag_results.append((g, mol_id, f"FAIL: {msg}"))
            except (OSError, subprocess.TimeoutExpired) as exc:
                tag_results.append((g, mol_id, f"FAIL: {exc.__class__.__name__}"))

# -----------------------------------------------------------------
# Step 6: render report.
# -----------------------------------------------------------------
total_cited = sum(len(ids) for ids in cited_pending.values())
citing_galaxies = sorted({
    g_citing
    for g, ids in cited_pending.items()
    for refs in ids.values()
    for g_citing, _ln in refs
})
mode = "dry-run" if dry_run else "applied"

emit(f"# curate-syzygie-check report — mode: {mode}")
emit(f"# generated_at: {os.popen('date -u +%FT%TZ').read().strip()}")
emit(f"# galaxies_root: {galaxies_root}")
emit(f"# scope: {','.join(scope)}")
emit("")
emit("## Counters")
emit(f"- molecules_cited: {total_cited}")
emit(f"- galaxies_citing: {len(citing_galaxies)}  ({', '.join(citing_galaxies) or '—'})")
emit(f"- orphan_citations: {len(orphans)}")
emit("")
emit("## Pending molecules cited cross-galaxy (must carry syzygie tag)")
if total_cited == 0:
    emit("(none — no cross-galaxy citation pressure on the current pending backlog)")
else:
    for g in scope:
        ids = cited_pending[g]
        if not ids:
            continue
        emit(f"### {g}")
        for mol_id in sorted(ids.keys()):
            citers = ids[mol_id]
            citer_summary = ", ".join(
                f"{cg}:L{ln}" for cg, ln in sorted(set(citers))
            )
            emit(f"- `{mol_id}`  ← cited by: {citer_summary}")
emit("")
emit("## Orphan citations (chronicle/ADR references a non-existent molecule)")
if not orphans:
    emit("(none — every cited ID resolves to a real molecule)")
else:
    for cited_id, fpath, lineno, ctx in orphans:
        emit(f"- `{cited_id}` — {fpath}:{lineno}")
        emit(f"    {ctx[:160]}")
emit("")
emit("## Tag application")
if dry_run:
    emit("(dry-run — re-invoke with --apply to write tags)")
else:
    if not tag_results:
        emit("(nothing to tag — cited_pending was empty)")
    else:
        for g, mol_id, result in tag_results:
            emit(f"- {g} {mol_id}: {result}")

# Exit code: orphans → 1, otherwise 0.
sys.exit(1 if orphans else 0)
PY
