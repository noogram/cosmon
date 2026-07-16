#!/usr/bin/env bash
# peek.sh — fleet triage via tmux + jq + fzf (torvalds hedge: bash before ratatui)
#
# Lists every tmux session across every cosmon socket under /tmp/tmux-$UID/,
# joins with cosmon molecule state when the session name ends in a molecule
# short-id (last 4 hex of <kind>-<date>-<xxxx>), and lets you fzf-navigate.
# Enter attaches. The preview pane runs `tmux capture-pane -p`.
#
# Usage:
#   scripts/peek.sh              # interactive fzf picker
#   scripts/peek.sh --list       # plain tsv (socket, session, mol-id, status, step, age)
#   scripts/peek.sh --json       # ndjson of the same
#
# Dependencies: tmux, jq, fzf (optional for non-interactive modes).

set -euo pipefail

UID_NUM="$(id -u)"
TMUX_ROOT="/private/tmp/tmux-${UID_NUM}"
[ -d "$TMUX_ROOT" ] || TMUX_ROOT="/tmp/tmux-${UID_NUM}"

# Walk up to find .cosmon state dir
find_state_root() {
  local d="$PWD"
  while [ "$d" != "/" ]; do
    if [ -d "$d/.cosmon/state/fleets" ]; then
      echo "$d/.cosmon/state/fleets"
      return 0
    fi
    d="$(dirname "$d")"
  done
  # Fallback: repo root guess
  echo "$HOME/dev/projects/cosmon/.cosmon/state/fleets"
}

STATE_ROOT="$(find_state_root)"

now_epoch=$(date +%s)

# Build an index: short-id (last 4) -> "full-id|fleet|status|step|updated_epoch"
build_mol_index() {
  [ -d "$STATE_ROOT" ] || return 0
  for fleet_dir in "$STATE_ROOT"/*/; do
    [ -d "$fleet_dir" ] || continue
    local fleet
    fleet="$(basename "$fleet_dir")"
    for mol_dir in "$fleet_dir"molecules/*/; do
      [ -d "$mol_dir" ] || continue
      local sj="${mol_dir}state.json"
      [ -f "$sj" ] || continue
      local full short
      full="$(basename "$mol_dir")"
      short="${full##*-}"
      # Status, current_step, updated_at
      jq -r --arg fleet "$fleet" --arg short "$short" --arg full "$full" '
        [$short, $full, $fleet, (.status // "?"),
         ((.current_step|tostring) + "/" + (.total_steps|tostring)),
         (.updated_at // "")] | @tsv' "$sj" 2>/dev/null
    done
  done
}

MOL_INDEX="$(build_mol_index || true)"

age_seconds() {
  local ts="$1"
  [ -z "$ts" ] && { echo "-"; return; }
  local epoch
  epoch=$(date -j -f "%Y-%m-%dT%H:%M:%S" "${ts%%.*}" +%s 2>/dev/null || echo "")
  [ -z "$epoch" ] && { echo "-"; return; }
  local delta=$(( now_epoch - epoch ))
  if   [ "$delta" -lt 60 ];     then echo "${delta}s"
  elif [ "$delta" -lt 3600 ];   then echo "$((delta/60))m"
  elif [ "$delta" -lt 86400 ];  then echo "$((delta/3600))h"
  else                                echo "$((delta/86400))d"
  fi
}

# Emit rows: socket \t session \t mol-id \t status \t step \t age \t created
emit_rows() {
  [ -d "$TMUX_ROOT" ] || return 0
  for sock_path in "$TMUX_ROOT"/*; do
    [ -S "$sock_path" ] || continue
    local sock
    sock="$(basename "$sock_path")"
    # Only cosmon-flavored sockets (keep "cosmon*" + project sockets; skip tests)
    case "$sock" in
      cosmon-test-*) continue ;;
    esac
    tmux -S "$sock_path" list-sessions \
      -F '#{session_name}	#{session_created}' 2>/dev/null | \
    while IFS=$'\t' read -r sname screated; do
      local short="${sname##*-}"
      local mol="-" status="-" step="-" updated=""
      if [ -n "$MOL_INDEX" ]; then
        local hit
        hit="$(printf '%s\n' "$MOL_INDEX" | awk -F'\t' -v s="$short" '$1==s {print; exit}')"
        if [ -n "$hit" ]; then
          mol="$(printf '%s' "$hit"    | cut -f2)"
          status="$(printf '%s' "$hit" | cut -f4)"
          step="$(printf '%s' "$hit"   | cut -f5)"
          updated="$(printf '%s' "$hit" | cut -f6)"
        fi
      fi
      local age
      if [ -n "$updated" ]; then
        age="$(age_seconds "$updated")"
      else
        age=$(( now_epoch - screated ))
        if   [ "$age" -lt 60 ];    then age="${age}s"
        elif [ "$age" -lt 3600 ];  then age="$((age/60))m"
        elif [ "$age" -lt 86400 ]; then age="$((age/3600))h"
        else                            age="$((age/86400))d"
        fi
      fi
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$sock" "$sname" "$mol" "$status" "$step" "$age"
    done
  done
}

case "${1:-}" in
  --list)
    printf 'SOCKET\tSESSION\tMOLECULE\tSTATUS\tSTEP\tAGE\n'
    emit_rows
    exit 0
    ;;
  --json)
    emit_rows | jq -Rn '
      [inputs | split("\t") | {
        socket: .[0], session: .[1], molecule: .[2],
        status: .[3], step: .[4], age: .[5]
      }] | .[]'
    exit 0
    ;;
  -h|--help)
    sed -n '2,20p' "$0"; exit 0
    ;;
esac

command -v fzf >/dev/null || { echo "fzf not installed — try --list or --json" >&2; exit 2; }

# Interactive picker. Columnize for readability; keep socket+session in row for preview/attach.
ROWS="$(emit_rows | awk -F'\t' 'BEGIN{OFS="\t"}
  {printf "%-18s  %-45s  %-24s  %-10s  %-5s  %-6s\t%s\t%s\n",
    $1, $2, $3, $4, $5, $6, $1, $2}')"

[ -z "$ROWS" ] && { echo "no sessions found under $TMUX_ROOT" >&2; exit 0; }

PICK="$(printf '%s\n' "$ROWS" | fzf \
  --header='SOCKET              SESSION                                        MOLECULE                  STATUS      STEP   AGE' \
  --delimiter='\t' \
  --with-nth=1 \
  --preview='tmux -S '"$TMUX_ROOT"'/{2} capture-pane -p -t {3} 2>/dev/null | tail -n 200' \
  --preview-window='right:60%:wrap' \
  --ansi)" || exit 0

SOCK="$(printf '%s' "$PICK" | awk -F'\t' '{print $2}')"
SESS="$(printf '%s' "$PICK" | awk -F'\t' '{print $3}')"
[ -z "$SOCK" ] || [ -z "$SESS" ] && exit 0

exec tmux -S "$TMUX_ROOT/$SOCK" attach -t "$SESS"
