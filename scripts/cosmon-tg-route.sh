#!/usr/bin/env bash
# cosmon-tg-route — RÉCEPTIONNISTE/ROUTEUR du canal Telegram cosmon multi-sessions.
#
# ── Rôle ────────────────────────────────────────────────────────────────────
# Le poller inbox-only (daemon `cosmon-bot`, un seul getUpdates par token —
# NE JAMAIS en lancer un 2e = 409) dépose chaque message entrant de l'opérateur
# dans  ~/Drop/cosmon-notifications/inbox/<ts>-<update_id>.txt . Ce routeur
# consomme les NOUVEAUX fichiers (marqueur d'offset, pas de re-traitement),
# parse l'ADRESSAGE, et route CHAQUE message vers la bonne session Claude via
# les primitives ensemble EXISTANTES :
#     cs whisper --to-session <sid>   (append .cosmon/state/presence/<sid>.log)
#     cs whisper <mol-id>             (paste dans le pane tmux d'un worker live)
#     cs drop                          (filet : nucléer un molecule si personne
#                                       n'écoute)
# Aucun système de messagerie parallèle n'est créé : le data plane reste le
# filesystem (invariant cosmon "no mailbox"). Le routeur n'est qu'un aiguilleur
# entre le fichier inbox et les canaux ensemble déjà en place.
#
# ── Adressage (convention) ──────────────────────────────────────────────────
#   @<sid-prefix>: msg   → route vers la session dont l'id contient <sid-prefix>
#   @<mol-id>: msg       → route vers le worker live de ce molecule (pane tmux)
#                          (mol-id = task-/idea-/decision-/issue-/…-YYYYMMDD-xxxx)
#   @all: msg            → broadcast à toutes les sessions fraîches
#   msg (non adressé)    → route vers la session la plus RÉCEMMENT active
#                          (heartbeat le plus frais dans la fenêtre de fraîcheur) ;
#                          si aucune → `cs drop` (le message devient un molecule).
# Le préfixe <sid8> émis par `cosmon-notify --from` est exactement la cible
# `@<sid8>:` que l'opérateur ré-adresse — la boucle se referme.
#
# ── Modes ───────────────────────────────────────────────────────────────────
#   cosmon-tg-route            # tick one-shot idempotent (traite le nouveau,
#                              #   met à jour le marqueur, sort) — schedulable
#   cosmon-tg-route --loop [S] # boucle continue (défaut S=3s) — pour daemon
#   cosmon-tg-route --dry-run  # décisions seulement (cs whisper --dry-run), rien
#                              #   n'est délivré, le marqueur N'est PAS avancé
#
# Kill switch : touch ~/.cosmon/cosmon-tg-route.off
#
# Overrides (tests) :
#   COSMON_TG_ROUTE_STATE_DIR   défaut /srv/cosmon/cosmon/.cosmon/state
#   COSMON_TG_ROUTE_INBOX       défaut ~/Drop/cosmon-notifications/inbox
#   COSMON_TG_ROUTE_MARKER      défaut ~/.cosmon/cosmon-tg-route.marker
#   COSMON_TG_ROUTE_FRESH_SECONDS  défaut 900 (fenêtre "session active")
#   COSMON_TG_ROUTE_CS          défaut "cs" (binaire cosmon ; stub-able en test)
set -uo pipefail

STATE_DIR="${COSMON_TG_ROUTE_STATE_DIR:-$HOME/galaxies/cosmon/.cosmon/state}"
INBOX="${COSMON_TG_ROUTE_INBOX:-$HOME/Drop/cosmon-notifications/inbox}"
MARKER="${COSMON_TG_ROUTE_MARKER:-$HOME/.cosmon/cosmon-tg-route.marker}"
FRESH_SECONDS="${COSMON_TG_ROUTE_FRESH_SECONDS:-900}"
CS="${COSMON_TG_ROUTE_CS:-cs}"
KILL="$HOME/.cosmon/cosmon-tg-route.off"
PRESENCE_DIR="$STATE_DIR/presence"

DRY=0
LOOP=0
INTERVAL=3
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --loop) LOOP=1; shift; case "${1:-}" in ''|--*) : ;; *) INTERVAL="$1"; shift ;; esac ;;
    -h|--help) sed -n '2,58p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "cosmon-tg-route: unknown arg $1" >&2; exit 2 ;;
  esac
done

log() { printf '%s cosmon-tg-route: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >&2; }

# ── Python helpers (JSON + temps + extraction texte) ─────────────────────────
# Un seul point d'entrée `_py <mode> [args...]`, appelé depuis bash.
_py() {
  python3 - "$@" <<'PY'
import sys, os, json, glob
from datetime import datetime, timezone

mode = sys.argv[1]

def load_sessions(presence_dir):
    out = []
    for p in glob.glob(os.path.join(presence_dir, "session-*.json")):
        try:
            with open(p, encoding="utf-8") as f:
                d = json.load(f)
        except Exception:
            continue
        sid = d.get("session_id")
        hb = d.get("heartbeat_at") or d.get("started_at")
        if not sid or not hb:
            continue
        try:
            ts = datetime.fromisoformat(hb.replace("Z", "+00:00"))
            if ts.tzinfo is None:
                ts = ts.replace(tzinfo=timezone.utc)
        except Exception:
            continue
        out.append((sid, ts))
    # freshest first
    out.sort(key=lambda x: x[1], reverse=True)
    return out

if mode == "freshest":
    presence_dir, window = sys.argv[2], float(sys.argv[3])
    now = datetime.now(timezone.utc)
    for sid, ts in load_sessions(presence_dir):
        if (now - ts).total_seconds() <= window:
            print(sid)
            break

elif mode == "all-fresh":
    presence_dir, window = sys.argv[2], float(sys.argv[3])
    now = datetime.now(timezone.utc)
    for sid, ts in load_sessions(presence_dir):
        if (now - ts).total_seconds() <= window:
            print(sid)

elif mode == "resolve":
    presence_dir, prefix, window = sys.argv[2], sys.argv[3], float(sys.argv[4])
    now = datetime.now(timezone.utc)
    # exact id containing prefix (session-<prefix> or raw prefix), freshest wins,
    # fresh sessions preferred but stale still resolvable (durable log).
    cands = [(sid, ts) for sid, ts in load_sessions(presence_dir)
             if prefix in sid or sid.replace("session-", "").startswith(prefix)]
    fresh = [c for c in cands if (now - c[1]).total_seconds() <= window]
    pick = (fresh or cands)
    if pick:
        print(pick[0][0])

elif mode == "text":
    path = sys.argv[2]
    try:
        lines = open(path, encoding="utf-8").read().splitlines()
    except Exception:
        sys.exit(0)
    for i, l in enumerate(lines):
        if l.startswith("text:"):
            rest = [l[len("text:"):].lstrip()] + lines[i + 1:]
            sys.stdout.write("\n".join(rest).rstrip("\n"))
            break
PY
}

# ── Delivery primitives ──────────────────────────────────────────────────────
whisper_session() { # <sid> <msg>
  local sid="$1" msg="$2"
  if [ "$DRY" -eq 1 ]; then
    "$CS" whisper --config "$STATE_DIR" --to-session "$sid" -m "$msg" --dry-run
    log "DRY route → session $sid"
  elif "$CS" whisper --config "$STATE_DIR" --to-session "$sid" -m "$msg" >/dev/null; then
    log "route → session $sid"
  else
    log "FAILED route → session $sid"
  fi
}

whisper_mol() { # <mol-id> <msg>
  local mol="$1" msg="$2"
  if [ "$DRY" -eq 1 ]; then
    "$CS" whisper --config "$STATE_DIR" "$mol" -m "$msg" --dry-run
    log "DRY route → molecule $mol"
  elif "$CS" whisper --config "$STATE_DIR" "$mol" -m "$msg" >/dev/null; then
    log "route → molecule $mol"
  else
    log "FAILED route → molecule $mol (worker offline?)"
  fi
}

drop_msg() { # <msg>
  local msg="$1"
  if [ "$DRY" -eq 1 ]; then
    log "DRY fallback → cs drop"
    return 0
  fi
  if "$CS" drop --config "$STATE_DIR" --tag source:telegram "$msg" >/dev/null; then
    log "fallback → cs drop (no live session)"
  else
    log "FAILED fallback → cs drop"
  fi
}

is_mol_id() { # <target>
  printf '%s' "$1" | grep -qE '^(task|idea|decision|issue|signal|deliberation|delib|spark|mol)-[0-9]{8}-[0-9a-f]{4}$'
}

# ── Route one message (already-extracted text) ───────────────────────────────
route_message() {
  local text="$1"
  [ -z "$text" ] && { log "empty message, skipped"; return 0; }

  local target="" body="$text"
  if [[ "$text" =~ ^@([A-Za-z0-9._-]+):[[:space:]]*(.*) ]]; then
    target="${BASH_REMATCH[1]}"
    local first_rest="${BASH_REMATCH[2]}"
    if [[ "$text" == *$'\n'* ]]; then
      body="${first_rest}"$'\n'"${text#*$'\n'}"
    else
      body="$first_rest"
    fi
  fi

  if [ -z "$target" ]; then
    # Non adressé → session la plus récemment active, sinon drop.
    local sid
    sid="$(_py freshest "$PRESENCE_DIR" "$FRESH_SECONDS")"
    if [ -n "$sid" ]; then
      whisper_session "$sid" "$body"
    else
      drop_msg "$body"
    fi
    return 0
  fi

  case "$target" in
    all|broadcast|everyone|tous)
      local any=0 s
      while IFS= read -r s; do
        [ -z "$s" ] && continue
        whisper_session "$s" "$body"; any=1
      done < <(_py all-fresh "$PRESENCE_DIR" "$FRESH_SECONDS")
      [ "$any" -eq 0 ] && drop_msg "$body"
      ;;
    *)
      if is_mol_id "$target"; then
        whisper_mol "$target" "$body"
      else
        local sid
        sid="$(_py resolve "$PRESENCE_DIR" "$target" "$FRESH_SECONDS")"
        if [ -n "$sid" ]; then
          whisper_session "$sid" "$body"
        else
          log "no session matches @$target — fallback drop"
          drop_msg "$body"
        fi
      fi
      ;;
  esac
}

# ── One idempotent tick: process inbox files newer than the marker ───────────
tick() {
  [ -f "$KILL" ] && return 0
  [ -d "$INBOX" ] || return 0

  local last=""
  [ -f "$MARKER" ] && last="$(cat "$MARKER" 2>/dev/null || true)"

  local processed="$last" f base text
  # Lexical sort == chronological (filenames are <ts>-<update_id>.txt).
  for f in "$INBOX"/*.txt; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    # Skip anything at-or-before the marker.
    if [ -n "$last" ] && [ ! "$base" \> "$last" ]; then
      continue
    fi
    text="$(_py text "$f")"
    log "inbox $base → routing"
    route_message "$text"
    if [ -z "$processed" ] || [ "$base" \> "$processed" ]; then
      processed="$base"
    fi
  done

  # Advance marker only in live mode (dry-run must be repeatable).
  if [ "$DRY" -eq 0 ] && [ -n "$processed" ] && [ "$processed" != "$last" ]; then
    mkdir -p "$(dirname "$MARKER")"
    printf '%s' "$processed" > "$MARKER"
  fi
}

if [ "$LOOP" -eq 1 ]; then
  log "loop mode (interval ${INTERVAL}s), inbox=$INBOX state=$STATE_DIR"
  while true; do
    [ -f "$KILL" ] && { log "kill switch present, standing down"; exit 0; }
    tick
    sleep "$INTERVAL"
  done
else
  tick
fi
