#!/usr/bin/env bash
# cosmon-state-backup.sh — ONE idempotent cold-copy pass of the sovereign
# cosmon live-state tree from fast LOCAL scratch (/home/tmp, reboot-wipeable)
# to durable NFS $HOME. C2 of delib-20260705-7288 (torvalds Q8 durability leg).
#
# WHY a cold copy and not "just keep state on NFS": NFS flock/fcntl silently
# violate the ADR-052/ADR-131 single-writer ledger guarantee → corrupt state.
# So the LIVE ledger lives on /home/tmp (local, correct locking) and this script
# periodically mirrors it to NFS purely for reboot survival. The NFS copy is a
# BACKUP, never the working set — nothing ever writes the ledger over NFS.
#
# Called by provision.sh's systemd --user timer or its guarded nohup loop, every
# ~5 min. Safe to run standalone or by hand.
#
# Idempotent: rsync only ships changed blocks; a second run with no state change
# copies nothing. `--delete` keeps the mirror a faithful copy (removals mirror).
#
# Usage:
#   cosmon-state-backup.sh [--src DIR] [--dst DIR] [--no-delete] [--quiet]
#     --src DIR    LOCAL state root.   Default: /home/tmp/$USER/cosmon-state
#     --dst DIR    NFS mirror root.    Default: $HOME/.cosmon-state-backup
#     --no-delete  Do not mirror deletions (keep an additive backup).
#     --quiet      Suppress the per-run summary line.
#
# Exit: 0 mirrored · 2 usage · 3 src missing.
set -euo pipefail

SRC="/home/tmp/$USER/cosmon-state"
DST="$HOME/.cosmon-state-backup"
DELETE="--delete"
QUIET=0

while [ $# -gt 0 ]; do
  case "$1" in
    --src)       SRC="$2"; shift 2 ;;
    --dst)       DST="$2"; shift 2 ;;
    --no-delete) DELETE=""; shift ;;
    --quiet)     QUIET=1; shift ;;
    -h|--help)   sed -n '1,30p' "$0"; exit 0 ;;
    *) echo "cosmon-state-backup: unknown arg: $1" >&2; exit 2 ;;
  esac
done

[ -d "$SRC" ] || { echo "cosmon-state-backup: src not found: $SRC" >&2; exit 3; }
mkdir -p "$DST"

# Exclude volatile lockfiles / pids / ptys — they are meaningless off the local
# host and only cause rsync churn. The durable ledger is events.jsonl + the
# tracked markdown artifacts; state.json is a derivable cache but cheap to carry.
rsync -a $DELETE \
  --exclude '*.lock' \
  --exclude '*.pid' \
  --exclude 'runtime.lock' \
  --exclude 'pty.log' \
  --exclude '.backup-loop.pid' \
  "$SRC"/ "$DST"/

if [ "$QUIET" = 0 ]; then
  # No `date` dependency assumptions: use rsync's own stats-free summary.
  bytes="$(du -sh "$DST" 2>/dev/null | awk '{print $1}')" || bytes="?"
  echo "cosmon-state-backup: mirrored $SRC -> $DST (mirror size ~$bytes)"
fi
