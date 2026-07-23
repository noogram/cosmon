#!/usr/bin/env bash
# trace-sidecar.sh — always-on, compile-independent trace of a cosmon polymer.
#
# WHY THIS EXISTS (and why it is a shell/python sidecar, not a Rust organ):
# A polymer's gate DAG can stall, a worker can die, or the Rust build itself can
# be broken (this tool was born from COSMON-DEV #20, where the claude adapter
# would not run inside a root container at all). A tracer whose job is to let a
# third party reconstruct "what actually ran" must NOT depend on the thing that
# might be broken. So it reads only the append-only event log and the on-disk
# molecule state — both of which exist the instant the polymer germinates,
# independent of whether any node compiles or completes.
#
# It is strictly READ-ONLY on .cosmon/state/: it copies event lines and hashes
# artifact bytes, and writes exclusively into its own --out directory (by
# default the root molecule's own trace/ subdir). It never mutates state.
#
# Emits three files (see docs/guides/trace-sidecar.md):
#   trace/events.jsonl  append-only, deduped-by-seq copy of this polymer's events
#   trace/briefs.md     each node's germinated brief (topic + formula + kind)
#   trace/hashes.tsv    sha256 + byte count for every artifact any node writes
#
# Usage:
#   scripts/trace-sidecar.sh --mol <root-molecule-id> [options]
#
# Options:
#   --mol <id>       Root molecule of the polymer to trace. Required unless
#                    $COSMON_MOLECULE_ID is set.
#   --fleet <id>     Fleet id (default: default).
#   --state <dir>    Path to the .cosmon/state root. Default: walk up from CWD.
#   --out <dir>      Where to write the trace. Default:
#                    <state>/fleets/<fleet>/molecules/<mol>/trace
#   -h, --help       Print this help and exit.
#
# The trace is idempotent: events.jsonl is appended (never rewritten), briefs.md
# and hashes.tsv are regenerated snapshots of current state. Re-run it as a tick
# to refresh; it is safe to run at any cadence, including after a crash.
set -euo pipefail

usage() { sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; }

MOL="${COSMON_MOLECULE_ID:-}"
FLEET="default"
STATE=""
OUT=""

while [ $# -gt 0 ]; do
  case "$1" in
    --mol)   MOL="${2:?--mol needs a value}"; shift 2 ;;
    --fleet) FLEET="${2:?--fleet needs a value}"; shift 2 ;;
    --state) STATE="${2:?--state needs a value}"; shift 2 ;;
    --out)   OUT="${2:?--out needs a value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "trace-sidecar: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ -z "$MOL" ]; then
  echo "trace-sidecar: --mol <id> is required (or set \$COSMON_MOLECULE_ID)" >&2
  exit 2
fi

# Walk up from CWD to locate .cosmon/state if not given explicitly.
if [ -z "$STATE" ]; then
  dir="$PWD"
  while [ "$dir" != "/" ]; do
    if [ -d "$dir/.cosmon/state" ]; then STATE="$dir/.cosmon/state"; break; fi
    dir="$(dirname "$dir")"
  done
fi
if [ -z "$STATE" ] || [ ! -d "$STATE" ]; then
  echo "trace-sidecar: could not locate .cosmon/state (use --state)" >&2
  exit 2
fi

if [ -z "$OUT" ]; then
  OUT="$STATE/fleets/$FLEET/molecules/$MOL/trace"
fi
mkdir -p "$OUT"

# The whole capture is one python3 pass: json parsing in bash is a footgun and
# a sidecar must be dependable. python3 is present on every cosmon host.
STATE="$STATE" FLEET="$FLEET" MOL="$MOL" OUT="$OUT" python3 - <<'PY'
import hashlib
import json
import os
import sys
from pathlib import Path

STATE = Path(os.environ["STATE"])
FLEET = os.environ["FLEET"]
ROOT = os.environ["MOL"]
OUT = Path(os.environ["OUT"])

mol_dir = STATE / "fleets" / FLEET / "molecules"

# ── 1. Discover polymer membership ──────────────────────────────────────────
# The polymer is the root molecule plus every node reachable through
# provenance / progression edges: foaming (Decay*), merges (Merged*), and the
# DAG gate edges (Blocks / BlockedBy). Refines/Refutes/Entangled are semantic
# citations, not membership, and are deliberately excluded.
MEMBERSHIP_EDGES = {
    "decayed_from": ("id",),
    "decay_product": ("id",),
    "merged_from": ("ids",),
    "merged_into": ("id",),
    "blocks": ("target",),
    "blocked_by": ("source",),
}


def load_state(mid):
    p = mol_dir / mid / "state.json"
    if not p.is_file():
        return None
    try:
        return json.loads(p.read_text())
    except (OSError, ValueError):
        return None


def neighbours(state):
    out = []
    for link in state.get("links", []) or []:
        rel = link.get("rel")
        for field in MEMBERSHIP_EDGES.get(rel, ()):
            val = link.get(field)
            if isinstance(val, list):
                out.extend(val)
            elif isinstance(val, str):
                out.append(val)
    return out


members = []
seen = set()
frontier = [ROOT]
while frontier:
    mid = frontier.pop()
    if mid in seen:
        continue
    seen.add(mid)
    st = load_state(mid)
    if st is None:
        # A member id with no on-disk state (e.g. a cross-galaxy or not-yet-
        # germinated node) is still recorded — its absence is itself evidence.
        members.append((mid, None))
        continue
    members.append((mid, st))
    frontier.extend(neighbours(st))

member_ids = {mid for mid, _ in members}

# ── 2. events.jsonl — append-only, deduped by seq ───────────────────────────
# Match on any of the id fields the EventV2 log uses across variants.
ID_FIELDS = ("molecule_id", "mol_id", "mol", "id", "molecule")


def event_touches_polymer(ev):
    for f in ID_FIELDS:
        v = ev.get(f)
        if isinstance(v, str) and v in member_ids:
            return True
    return False


# Scan the live log plus any rolled archive shards.
sources = [STATE / "events.jsonl"]
archive = STATE / "archive" / "events"
if archive.is_dir():
    sources.extend(sorted(archive.glob("*.jsonl")))

collected = {}  # seq -> raw line (canonicalised)
for src in sources:
    if not src.is_file():
        continue
    with src.open() as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            if not event_touches_polymer(ev):
                continue
            seq = ev.get("seq")
            key = seq if seq is not None else f"noseq:{hash(line)}"
            collected[key] = json.dumps(ev, separators=(",", ":"), sort_keys=True)

events_path = OUT / "events.jsonl"
already = set()
if events_path.is_file():
    with events_path.open() as fh:
        for line in fh:
            try:
                already.add(json.loads(line).get("seq"))
            except ValueError:
                continue


def seq_sort_key(k):
    return (0, k) if isinstance(k, int) else (1, str(k))


appended = 0
with events_path.open("a") as fh:
    for key in sorted(collected, key=seq_sort_key):
        seq = key if isinstance(key, int) else None
        if seq is not None and seq in already:
            continue
        fh.write(collected[key] + "\n")
        appended += 1

# ── 3. briefs.md — each node's germinated brief ─────────────────────────────
lines = [
    "# Polymer trace — germinated briefs",
    "",
    f"Root molecule: `{ROOT}`  ·  fleet: `{FLEET}`",
    f"Nodes discovered: {len(members)}",
    "",
    "Regenerated snapshot (see events.jsonl for the append-only timeline).",
    "",
]
for mid, st in sorted(members, key=lambda m: m[0]):
    lines.append(f"## `{mid}`")
    lines.append("")
    if st is None:
        lines.append("_No on-disk state — member by link only (not yet "
                     "germinated, or lives in another galaxy)._")
        lines.append("")
        continue
    vars_ = st.get("variables", {}) or {}
    topic = vars_.get("topic") or vars_.get("question") or vars_.get("formula") or "(none)"
    lines.append(f"- **formula:** `{st.get('formula_id', '?')}`")
    lines.append(f"- **status:** `{st.get('status', '?')}`  ·  "
                 f"step {st.get('current_step', '?')}/{st.get('total_steps', '?')}")
    lines.append(f"- **created:** `{st.get('created_at', '?')}`")
    tags = st.get("tags") or []
    if tags:
        lines.append(f"- **tags:** {', '.join('`%s`' % t for t in tags)}")
    links = st.get("links") or []
    if links:
        rels = ", ".join(sorted({l.get("rel", "?") for l in links}))
        lines.append(f"- **links:** {rels}")
    lines.append("- **topic:**")
    lines.append("")
    for tl in str(topic).strip().splitlines() or [""]:
        lines.append(f"  > {tl}")
    lines.append("")
(OUT / "briefs.md").write_text("\n".join(lines) + "\n")

# ── 4. hashes.tsv — content hash + byte count per artifact ──────────────────
# Every file any node wrote into its molecule dir is an artifact. We skip our
# own --out subtree so the trace never hashes (or races) itself.
out_resolved = OUT.resolve()
rows = ["mol_id\trel_path\tbytes\tsha256"]
for mid, st in sorted(members, key=lambda m: m[0]):
    base = mol_dir / mid
    if not base.is_dir():
        continue
    for path in sorted(base.rglob("*")):
        if not path.is_file():
            continue
        try:
            if out_resolved in path.resolve().parents or path.resolve() == out_resolved:
                continue
        except OSError:
            continue
        try:
            data = path.read_bytes()
        except OSError:
            continue
        digest = hashlib.sha256(data).hexdigest()
        rel = path.relative_to(base).as_posix()
        rows.append(f"{mid}\t{rel}\t{len(data)}\t{digest}")
(OUT / "hashes.tsv").write_text("\n".join(rows) + "\n")

print(
    f"trace-sidecar: {len(members)} node(s), "
    f"+{appended} new event(s) (total captured {len(collected)}), "
    f"{len(rows) - 1} artifact(s) hashed -> {OUT}",
    file=sys.stderr,
)
PY
