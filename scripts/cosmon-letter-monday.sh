#!/usr/bin/env bash
# cosmon-letter-monday.sh — godin ship-moment #1: the Monday morning letter.
#
# One artifact delivered on a real cadence that says "I've been paying
# attention even while you were busy." Composed mechanically from the
# cosmon archive + inbox + chronicles. Sent Monday 08:30 local via
# osascript + Mail.app to dev@noogram.dev.
#
# Provenance: delib-20260423-95fe (godin §5, ship moment 1) →
# task-20260423-e9cf (this script).
#
# Usage:
#   cosmon-letter-monday.sh --dry-run     print letter to stdout
#   cosmon-letter-monday.sh               compose + send + record
#   cosmon-letter-monday.sh --force       re-send even if today has a record
#
# Idempotency: records the sent letter at
#   .cosmon/state/letters/<YYYY-MM-DD>.md
# A second invocation on the same day is a no-op (exit 0) unless --force.
#
# Failure mode (by design): if osascript/Mail.app fails, we log to
#   .cosmon/state/letters/errors.log and exit 1. No retry. Missed weeks
#   are silence, and silence is information — see briefing §"Delivery".

set -euo pipefail

MODE="send"
FORCE=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) MODE="dry-run" ;;
        --force)   FORCE=1 ;;
        -h|--help)
            sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "cosmon-letter-monday: unknown flag: $1" >&2
            exit 1
            ;;
    esac
    shift
done

# ---------------------------------------------------------------------------
# Locate the cosmon galaxy root. Precedence:
#   1. $COSMON_ROOT env override (honoured verbatim).
#   2. Walk-up from $PWD — lets the operator `cd /srv/cosmon/cosmon && …`.
#   3. $HOME/galaxies/cosmon — the canonical install path.
#   4. Walk-up from the script directory (last resort).
# We need `.cosmon/state/archive` and `docs/lore/CHRONICLES.md` on disk.
# ---------------------------------------------------------------------------
find_root_from() {
    local d="$1"
    while [[ "$d" != "/" && -n "$d" ]]; do
        if [[ -d "$d/.cosmon/state/archive" && -f "$d/docs/lore/CHRONICLES.md" ]]; then
            printf '%s' "$d"
            return 0
        fi
        d="$(dirname "$d")"
    done
    return 1
}

COSMON_ROOT=""
if [[ -n "${COSMON_ROOT_OVERRIDE:-}" ]]; then
    COSMON_ROOT="$COSMON_ROOT_OVERRIDE"
elif found="$(find_root_from "$PWD" 2>/dev/null)"; then
    COSMON_ROOT="$found"
elif [[ -d "$HOME/galaxies/cosmon/.cosmon/state/archive" ]]; then
    COSMON_ROOT="$HOME/galaxies/cosmon"
else
    SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
    COSMON_ROOT="$(find_root_from "$SCRIPT_DIR" 2>/dev/null || true)"
fi
if [[ -z "$COSMON_ROOT" ]]; then
    echo "cosmon-letter-monday: cannot locate cosmon galaxy root" >&2
    exit 1
fi

STATE_DIR="$COSMON_ROOT/.cosmon/state/letters"
ERROR_LOG="$STATE_DIR/errors.log"
TO="dev@noogram.dev"

TODAY="$(date +%Y-%m-%d)"
WEEK_START_SEC=$(($(date -u +%s) - 7 * 86400))
WEEK_START_LABEL="$(date -v-7d +%d\ %B\ %Y 2>/dev/null || date -d '7 days ago' +'%d %B %Y')"
WEEK_END_LABEL="$(date +%d\ %B\ %Y)"
RECORD="$STATE_DIR/$TODAY.md"

mkdir -p "$STATE_DIR"

# ---------------------------------------------------------------------------
# completions_last_week — scan archive, return up to 5 lines
#   "<mol_id> · <formula> · <short topic>"
# sorted by merged_at desc. Uses python3 for JSON iteration; python3 is a
# hard dependency (shipped with macOS 14+).
# ---------------------------------------------------------------------------
completions_last_week() {
    python3 - "$COSMON_ROOT" "$WEEK_START_SEC" <<'PY'
import json
import os
import sys
import time
from datetime import datetime

root = sys.argv[1]
cutoff = int(sys.argv[2])

rows = []
archive = os.path.join(root, ".cosmon", "state", "archive")
if not os.path.isdir(archive):
    sys.exit(0)

for year in sorted(os.listdir(archive)):
    ypath = os.path.join(archive, year)
    if not os.path.isdir(ypath):
        continue
    for month in sorted(os.listdir(ypath)):
        mpath = os.path.join(ypath, month)
        if not os.path.isdir(mpath):
            continue
        for entry in os.listdir(mpath):
            mol_path = os.path.join(mpath, entry, "molecule.json")
            if not os.path.isfile(mol_path):
                continue
            try:
                with open(mol_path, encoding="utf-8") as f:
                    mol = json.load(f)
            except (OSError, json.JSONDecodeError):
                continue
            if mol.get("status") != "completed":
                continue
            merged = mol.get("merged_at")
            if not merged:
                continue
            try:
                dt = datetime.fromisoformat(merged.replace("Z", "+00:00"))
            except ValueError:
                continue
            ts = int(dt.timestamp())
            if ts < cutoff:
                continue
            rows.append((ts, mol))

rows.sort(key=lambda r: r[0], reverse=True)

def short_topic(mol):
    """Best-effort one-line summary of the molecule."""
    v = mol.get("variables") or {}
    for key in ("topic", "pattern", "question", "title", "name"):
        s = v.get(key)
        if isinstance(s, str) and s.strip():
            first = s.strip().split("\n", 1)[0]
            return (first[:100] + "…") if len(first) > 100 else first
    for val in v.values():
        if isinstance(val, str) and val.strip():
            first = val.strip().split("\n", 1)[0]
            return (first[:100] + "…") if len(first) > 100 else first
    return ""

for _, mol in rows[:5]:
    mol_id = mol.get("id", "?")
    formula = mol.get("formula_id") or mol.get("kind") or "?"
    topic = short_topic(mol)
    if topic:
        print(f"{mol_id} · {formula} · {topic}")
    else:
        print(f"{mol_id} · {formula}")
PY
}

# ---------------------------------------------------------------------------
# oldest_hot_priority — read `cs inbox --json`, pick the temp:hot pending
# molecule with the largest age_seconds. Empty output if none.
# ---------------------------------------------------------------------------
oldest_hot_priority() {
    if ! command -v cs >/dev/null 2>&1; then
        return 0
    fi
    ( cd "$COSMON_ROOT" && cs inbox --json 2>/dev/null ) | python3 -c '
import json
import sys

best = None
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        row = json.loads(line)
    except json.JSONDecodeError:
        continue
    if row.get("kind") != "row":
        continue
    tags = row.get("tags") or []
    if "temp:hot" not in tags:
        continue
    age = row.get("age_seconds") or 0
    if best is None or age > best.get("age_seconds", 0):
        best = row

if best:
    mol_id = best.get("mol_id", "?")
    topic = (best.get("topic") or "").strip().split("\n", 1)[0]
    if len(topic) > 100:
        topic = topic[:100] + "…"
    print(mol_id)
    print(topic)
'
}

# ---------------------------------------------------------------------------
# recent_promise — scan chronicles authored in the last 7 days for a
# first-person future-tense sentence. Returns one line max, or empty.
# Loose heuristic by design (briefing §"Data sources"): better to print
# nothing than to hallucinate a commitment.
# ---------------------------------------------------------------------------
recent_promise() {
    local chronicles="$COSMON_ROOT/docs/lore"
    [[ -d "$chronicles" ]] || return 0

    python3 - "$chronicles" "$WEEK_START_SEC" <<'PY'
import os
import re
import sys

chronicles_dir = sys.argv[1]
cutoff = int(sys.argv[2])

# Future-tense / promise markers. First match wins.
patterns = [
    re.compile(r"[Jj]e (?:reviendrai|ferai|vais|voudrai[s]?|devrai[s]?|compte)[^.!?\n]{5,140}[.!?]"),
    re.compile(r"(?:On|Nous) (?:reviendr[ao]n?s?|ferons?|allons?|devrons?)[^.!?\n]{5,140}[.!?]"),
    re.compile(r"(?:I will|I'll|we will|we'll|plan to|should)[^.!?\n]{5,140}[.!?]"),
]

candidates = []
for name in sorted(os.listdir(chronicles_dir)):
    path = os.path.join(chronicles_dir, name)
    if not os.path.isfile(path) or not name.endswith(".md"):
        continue
    try:
        st = os.stat(path)
    except OSError:
        continue
    if st.st_mtime < cutoff:
        continue
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except OSError:
        continue
    for pat in patterns:
        m = pat.search(text)
        if m:
            sentence = m.group(0).strip().replace("\n", " ")
            if 20 < len(sentence) < 200:
                candidates.append((st.st_mtime, sentence, name))
            break

if candidates:
    candidates.sort(reverse=True)
    _, sentence, _ = candidates[0]
    print(sentence)
PY
}

# ---------------------------------------------------------------------------
# Compose the letter. Pure templating — no LLM, no smoothing. A stilted
# bullet is honest; a hallucinated one is not (briefing §"non-goals").
# ---------------------------------------------------------------------------
compose_subject() {
    echo "Cosmon — la semaine du $WEEK_START_LABEL au $WEEK_END_LABEL"
}

compose_body() {
    local completions priority_id priority_topic promise
    completions="$(completions_last_week)"

    local priority_raw
    priority_raw="$(oldest_hot_priority)"
    priority_id="$(printf '%s\n' "$priority_raw" | sed -n '1p')"
    priority_topic="$(printf '%s\n' "$priority_raw" | sed -n '2p')"

    promise="$(recent_promise)"

    echo "Bonjour Noogram,"
    echo ""

    if [[ -z "$completions" ]]; then
        echo "La semaine dernière, la flotte n'a rien livré de mergé — rien à livrer cette semaine."
    else
        echo "La semaine dernière, la flotte a fini :"
        while IFS= read -r line; do
            [[ -z "$line" ]] && continue
            echo "  • $line"
        done <<< "$completions"
    fi
    echo ""

    if [[ -n "$promise" ]]; then
        echo "Tu m'avais dit dans une chronique récente :"
        echo "  \"$promise\""
        echo "Si c'est toujours d'actualité, la porte est ouverte."
    else
        echo "Pas de promesse explicite à re-surfacer cette semaine."
    fi
    echo ""

    if [[ -n "$priority_id" ]]; then
        echo "Aujourd'hui, la molécule qui mérite d'être ouverte en premier :"
        if [[ -n "$priority_topic" ]]; then
            echo "  $priority_id — $priority_topic"
        else
            echo "  $priority_id"
        fi
        echo "  (critère : le plus ancien pending tagué temp:hot.)"
    else
        echo "Aucune molécule temp:hot en attente — le backlog respire."
    fi
    echo ""

    echo "Bonne semaine. Je te lis."
    echo "— cosmon"
}

SUBJECT="$(compose_subject)"
BODY="$(compose_body)"

if [[ "$MODE" == "dry-run" ]]; then
    echo "To: $TO"
    echo "Subject: $SUBJECT"
    echo ""
    echo "$BODY"
    exit 0
fi

# Idempotency gate: a record for today means we already composed + sent.
if [[ -f "$RECORD" && "$FORCE" -eq 0 ]]; then
    echo "cosmon-letter-monday: already sent today ($RECORD) — skip. Use --force to resend." >&2
    exit 0
fi

# Build the AppleScript. We escape backslashes and double quotes per the
# same rules used by mailroom-mailer (see crates/mailroom-mailer/src/lib.rs).
escape_applescript() {
    python3 - <<'PY' "$1"
import sys
s = sys.argv[1]
print(s.replace("\\", "\\\\").replace('"', '\\"'), end="")
PY
}

ESC_SUBJECT="$(escape_applescript "$SUBJECT")"
ESC_BODY="$(escape_applescript "$BODY")"
ESC_TO="$(escape_applescript "$TO")"

SCRIPT=$(cat <<EOF
tell application "Mail"
  set newMessage to make new outgoing message with properties {subject:"$ESC_SUBJECT", content:"$ESC_BODY", visible:false}
  tell newMessage
    make new to recipient at end of to recipients with properties {address:"$ESC_TO"}
    set sender to "$TO"
  end tell
  send newMessage
end tell
EOF
)

if osascript -e "$SCRIPT" >/dev/null 2>"$ERROR_LOG.tmp"; then
    {
        echo "---"
        echo "to: $TO"
        echo "subject: $SUBJECT"
        echo "sent_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "---"
        echo ""
        echo "$BODY"
    } > "$RECORD"
    rm -f "$ERROR_LOG.tmp"
    echo "cosmon-letter-monday: sent to $TO (record: $RECORD)"
else
    rc=$?
    {
        echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] osascript failed (rc=$rc)"
        cat "$ERROR_LOG.tmp" 2>/dev/null || true
    } >> "$ERROR_LOG"
    rm -f "$ERROR_LOG.tmp"
    echo "cosmon-letter-monday: send FAILED — see $ERROR_LOG" >&2
    exit 1
fi
