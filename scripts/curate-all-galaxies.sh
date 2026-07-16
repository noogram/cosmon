#!/usr/bin/env bash
# curate-all-galaxies.sh — nightly drain-patrol driver across the allowlist.
#
# Parent: delib-20260521-c3cd (cross-galaxy drain patrol architecture v0).
# Child:  task-20260521-9ee1 (this script, the LaunchAgent, the config TOML).
#
# Reads ~/.config/cosmon/curate.toml for the galaxy allowlist + per-night
# budget caps, then runs the cosmon `curate-patrol` formula serially in each
# galaxy listed under `[drain].galaxies`. One molecule per galaxy per night.
#
# Hard preconditions, checked before any per-galaxy work:
#   1. ~/.cosmon/autopilot.off — kill-switch. If present, exit 0 silently.
#   2. ~/.config/cosmon/curate.toml — config. If missing, exit 0 (no drift).
#
# Per-galaxy preconditions, checked inside the loop:
#   3. <galaxy>/.cosmon/ exists — otherwise the galaxy is not cosmon-managed.
#   4. No live peer session (heartbeat <3 min) — otherwise skip.
#
# Pass selection: odd day-of-year = pass 1 (retag), even = pass 2 (collapse).
# Pass is passed into the formula as `--var pass=<N>` for the worker to honour.
#
# Serial loop only. No `&`, no parallelism. Contention is cheap to avoid by
# serialising (carnot: irreversible coordination cost > sequential cost
# when N is small and the work is bounded).

set -euo pipefail

CFG="${COSMON_CURATE_CONFIG:-$HOME/.config/cosmon/curate.toml}"
KILL_SWITCH="${COSMON_AUTOPILOT_KILL_SWITCH:-$HOME/.cosmon/autopilot.off}"
LOG_DIR="$HOME/.cosmon"
LOG="$LOG_DIR/curate.log"
PEER_HEARTBEAT_THRESHOLD_SEC="${COSMON_PEER_HEARTBEAT_THRESHOLD_SEC:-180}"

mkdir -p "$LOG_DIR"

log() {
    printf '[curate-all] %s %s\n' "$(date -u +%FT%TZ)" "$*" >> "$LOG"
}

# --- precondition 1: kill-switch ------------------------------------------
if [[ -f "$KILL_SWITCH" ]]; then
    log "autopilot.off present at $KILL_SWITCH — exit"
    exit 0
fi

# --- precondition 2: config exists ----------------------------------------
if [[ ! -f "$CFG" ]]; then
    log "config not found at $CFG — exit (no drift, operator install pending)"
    exit 0
fi

# --- read allowlist + budgets from TOML -----------------------------------
# Python is the most portable way to parse TOML on macOS 14+ (tomllib in 3.11+).
# Falls back to grep if python3 lacks tomllib (rare; macOS 14 ships 3.9).
read_config_py() {
    /usr/bin/env python3 - "$CFG" <<'PY' 2>/dev/null
import sys
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib  # type: ignore[no-redef]
    except ImportError:
        sys.exit(2)

with open(sys.argv[1], "rb") as f:
    cfg = tomllib.load(f)

drain = cfg.get("drain", {})
galaxies = drain.get("galaxies") or []
budget = cfg.get("budget", {})

print("GALAXIES=" + " ".join(str(g) for g in galaxies))
print("MAX_COLLAPSE=" + str(budget.get("max_collapse_per_night", 1500)))
print("MAX_CLASSIFY=" + str(budget.get("max_classify_per_night", 200)))
print("MAX_TACKLE=" + str(budget.get("max_tackle_per_night", 0)))
print("MAX_SURFACE=" + str(budget.get("max_surface_per_night", 20)))
PY
}

read_config_grep() {
    # Minimal fallback: extracts only galaxies from `[drain]` section.
    local in_drain=0
    GALAXIES=""
    while IFS= read -r line; do
        if [[ "$line" =~ ^\[drain\] ]]; then in_drain=1; continue; fi
        if [[ "$line" =~ ^\[ ]] && [[ $in_drain -eq 1 ]]; then in_drain=0; fi
        if [[ $in_drain -eq 1 ]] && [[ "$line" =~ galaxies ]]; then
            GALAXIES=$(printf '%s' "$line" | grep -oE '"[a-zA-Z0-9_-]+"' | tr -d '"' | tr '\n' ' ')
        fi
    done < "$CFG"
    MAX_COLLAPSE=1500
    MAX_CLASSIFY=200
    MAX_TACKLE=0
    MAX_SURFACE=20
}

GALAXIES=""
MAX_COLLAPSE=""
MAX_CLASSIFY=""
MAX_TACKLE=""
MAX_SURFACE=""

if cfg_out=$(read_config_py); then
    eval "$cfg_out"
else
    log "python tomllib unavailable — falling back to grep parser"
    read_config_grep
fi

if [[ -z "$GALAXIES" ]]; then
    log "no galaxies in [drain].galaxies — exit"
    exit 0
fi

# --- pass selection: odd day-of-year = retag, even = collapse -------------
DAY=$(date -u +%j)
PASS=$(( DAY % 2 == 1 ? 1 : 2 ))

log "starting sweep pass=$PASS galaxies=[$GALAXIES] " \
    "caps[collapse=$MAX_COLLAPSE classify=$MAX_CLASSIFY " \
    "tackle=$MAX_TACKLE surface=$MAX_SURFACE]"

# --- peer-session guard: read presence files from filesystem --------------
# Source of truth is .cosmon/state/presence/*.json (one file per live
# session, refreshed by the session's heartbeat). We walk the filesystem
# directly rather than shelling out to `cs ensemble --all --json` because:
#   (a) cosmon's architectural-invariants say the filesystem is the
#       authoritative content channel — CLI is a view, not a source;
#   (b) filesystem read survives `cs ensemble` schema bugs or version skew.
# Returns 0 if any presence file in this galaxy has heartbeat < threshold.
peer_session_active() {
    local g_path="$1"
    local presence_dir="$g_path/.cosmon/state/presence"
    [[ -d "$presence_dir" ]] || return 1

    /usr/bin/env python3 - "$presence_dir" "$PEER_HEARTBEAT_THRESHOLD_SEC" <<'PY'
import json
import os
import sys
from datetime import datetime, timezone

presence_dir = sys.argv[1]
threshold_sec = int(sys.argv[2])
now = datetime.now(timezone.utc)

for name in os.listdir(presence_dir):
    if not name.startswith("session-") or not name.endswith(".json"):
        continue
    path = os.path.join(presence_dir, name)
    try:
        with open(path, encoding="utf-8") as f:
            row = json.load(f)
    except (OSError, json.JSONDecodeError):
        continue
    hb = row.get("heartbeat_at")
    if not hb:
        continue
    try:
        dt = datetime.fromisoformat(hb.replace("Z", "+00:00"))
    except ValueError:
        continue
    age = (now - dt).total_seconds()
    if age < threshold_sec:
        sys.exit(0)  # active peer
sys.exit(1)
PY
}

# --- per-galaxy loop ------------------------------------------------------
for G in $GALAXIES; do
    G_PATH="$HOME/galaxies/$G"

    if [[ ! -d "$G_PATH/.cosmon" ]]; then
        log "skip $G — no .cosmon at $G_PATH"
        continue
    fi

    if peer_session_active "$G_PATH"; then
        log "skip $G — live peer session (heartbeat <${PEER_HEARTBEAT_THRESHOLD_SEC}s)"
        continue
    fi

    log "→ $G pass=$PASS"

    # Re-check the kill-switch before each galaxy — operator may have
    # touched ~/.cosmon/autopilot.off mid-sweep.
    if [[ -f "$KILL_SWITCH" ]]; then
        log "autopilot.off appeared mid-sweep — abort remaining galaxies"
        exit 0
    fi

    if ! (
        cd "$G_PATH"
        ID=$(
            cs nucleate curate-patrol \
                --var galaxy="$G" \
                --var pass="$PASS" \
                --var max_collapse="$MAX_COLLAPSE" \
                --var max_classify="$MAX_CLASSIFY" \
                --var max_tackle="$MAX_TACKLE" \
                --var max_surface="$MAX_SURFACE" \
                --json \
            | /usr/bin/env python3 -c \
                'import json,sys; print(json.loads(sys.stdin.read()).get("id",""))'
        )
        if [[ -z "$ID" ]]; then
            echo "nucleate returned empty id" >&2
            exit 1
        fi
        cs tackle "$ID"
        cs wait "$ID"
        cs done "$ID"
    ) 2>>"$LOG"; then
        log "FAIL $G — see molecule logs + $LOG above"
    else
        log "OK $G"
    fi
done

log "sweep complete"
