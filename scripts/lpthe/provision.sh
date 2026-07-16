#!/usr/bin/env bash
# ╔══════════════════════════════════════════════════════════════════════════╗
# ║ provision.sh — converge a SOVEREIGN cosmon instance on an invited-guest    ║
# ║ host (g5 @ LPTHE Jussieu). C2 of delib-20260705-7288 ("Sovereign cosmon    ║
# ║ on LPTHE"). This is ADR-141's transport-agnostic BOOT CONTRACT minus       ║
# ║ crypto + containers (D2 "container-less avatar"): immutable base binary,   ║
# ║ idempotent self-config at boot, live state written only to fast LOCAL      ║
# ║ scratch. Native binary, NO container (D3 — podman rootless is dead on g5   ║
# ║ without /etc/subuid; see the C1 probe).                                    ║
# ╚══════════════════════════════════════════════════════════════════════════╝
#
# RUNS ON g5 (the guest host), NOT on the Mac. Shipped inside the release
# tarball by scripts/lpthe/ship-lpthe.sh and unpacked to $PREFIX.
#
# IDEMPOTENT by construction — safe to re-run. Every step checks its own
# post-condition first and is a no-op when already satisfied. This is the
# "converges an instance and is safe to re-run" contract from the C2 brief.
#
# C1 CONFIRMED FACTS this script relies on (evidence: delib-20260705-7288 / C1
# preflight GO):
#   • /home/tmp  = btrfs LOCAL on NVMe (1.9 TB, ~1.1 TB free), NO noexec,
#                  PERSISTENT across normal operation but reboot-WIPEABLE scratch.
#   • $HOME      = NFS (ada:/ada3) — durable, but NFS flock/fcntl make the
#                  ADR-052/ADR-131 single-writer ledger guarantee a LIE. Live
#                  cosmon state must therefore NEVER live on NFS.
#   • glibc 2.42 x86_64 — irrelevant: we ship a fully-static musl binary.
#   • NO /etc/subuid → podman rootless mort-né → native binary confirmed.
#   • ollama reachable at 127.0.0.1:11434 (native, no tunnel hop).
#
# STATE-PATH DECISION (D4 / C1): live `.cosmon/state` is a SYMLINK to
# /home/tmp/$USER/cosmon-state/<galaxy>/ (g5-local, single-writer-safe). The
# symlink is chosen over COSMON_STATE_DIR because the cosmon tmux server freezes
# its environment at creation (CLAUDE.md "tmux server env frozen at start"): an
# env var exported before `cs tackle` is silently dropped for later worker
# sessions, whereas a filesystem symlink is honoured by walk-up discovery
# regardless of env propagation. `.cosmon/state` is gitignored (ADR-030), so the
# symlink never dirties the tree. A `cosmon.env` with COSMON_STATE_DIR is also
# emitted as belt-and-suspenders for non-tmux one-shot `cs` invocations.
#
# DURABILITY (D4 / torvalds Q8): /home/tmp is reboot-wipeable, so a cold-copy
# rsync mirrors the local state tree to NFS $HOME every ~5 min. Git worktrees
# already live on NFS home; durability of *history* is git, durability of *live
# state* between reboots is this rsync.
#
# INVITED-GUEST DISCIPLINE (Dave: "don't use your framework to hack the lab"):
# this script escalates NOTHING. No sudo, no system units, no /etc writes, no
# network scanning. The rsync mirror runs as an unprivileged per-user loop
# (systemd --user timer if available, else a guarded nohup loop).
#
# Usage:
#   provision.sh [--galaxy NAME] [--repo DIR] [--prefix DIR]
#                [--state-root DIR] [--backup-root DIR]
#                [--interval SECONDS] [--no-backup] [--check-only]
#
#   --galaxy NAME     Galaxy name (state namespace).      Default: cosmon
#   --repo DIR        Galaxy git checkout on NFS $HOME.   Default: $HOME/galaxies/<galaxy>
#   --prefix DIR      Where the tarball is unpacked.      Default: dir of this script's parent
#   --state-root DIR  LOCAL live-state root.              Default: /home/tmp/$USER/cosmon-state
#   --backup-root DIR NFS cold-copy mirror root.          Default: $HOME/.cosmon-state-backup
#   --interval SEC    rsync cold-copy period.             Default: 300 (5 min)
#   --no-backup       Skip installing the rsync mirror (state is then volatile).
#   --check-only      Verify preconditions + report, mutate nothing.
#
# Exit: 0 converged (or check-only all-green) · 2 usage · 3 precondition failed.
set -euo pipefail

# ── defaults ────────────────────────────────────────────────────────────────
GALAXY="cosmon"
REPO=""
PREFIX=""
STATE_ROOT=""
BACKUP_ROOT=""
INTERVAL=300
DO_BACKUP=1
CHECK_ONLY=0

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── arg parse ───────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --galaxy)      GALAXY="$2"; shift 2 ;;
    --repo)        REPO="$2"; shift 2 ;;
    --prefix)      PREFIX="$2"; shift 2 ;;
    --state-root)  STATE_ROOT="$2"; shift 2 ;;
    --backup-root) BACKUP_ROOT="$2"; shift 2 ;;
    --interval)    INTERVAL="$2"; shift 2 ;;
    --no-backup)   DO_BACKUP=0; shift ;;
    --check-only)  CHECK_ONLY=1; shift ;;
    -h|--help)     sed -n '1,60p' "$0"; exit 0 ;;
    *) echo "provision: unknown arg: $1" >&2; exit 2 ;;
  esac
done

: "${PREFIX:=$(cd "$SELF/.." && pwd)}"                 # tarball root (parent of scripts/)
: "${REPO:=$HOME/galaxies/$GALAXY}"
: "${STATE_ROOT:=/home/tmp/$USER/cosmon-state}"
: "${BACKUP_ROOT:=$HOME/.cosmon-state-backup}"

BIN_DIR="$PREFIX/bin"
SHARE_DIR="$PREFIX/share"
CS="$BIN_DIR/cs"
MANIFEST="$PREFIX/MANIFEST.txt"
STATE_DIR="$STATE_ROOT/$GALAXY"

# ── ui helpers ──────────────────────────────────────────────────────────────
red()   { printf '\033[31mFAIL\033[0m  %s\n' "$1"; }
green() { printf '\033[32m OK \033[0m  %s\n' "$1"; }
warn()  { printf '\033[33mWARN\033[0m  %s\n' "$1"; }
step()  { printf '\n\033[1m==>\033[0m %s\n' "$1"; }
die()   { red "$1"; exit 3; }

# ── blake3 helper (b3sum if present, else python fallback) ──────────────────
b3() {
  if command -v b3sum >/dev/null 2>&1; then
    b3sum "$1" | awk '{print $1}'
  elif python3 -c 'import blake3' >/dev/null 2>&1; then
    python3 - "$1" <<'PY'
import sys, blake3
print(blake3.blake3(open(sys.argv[1],'rb').read()).hexdigest())
PY
  else
    echo "NO-B3SUM"
  fi
}

echo "cosmon sovereign provision — galaxy=$GALAXY prefix=$PREFIX"
echo "  state(local) = $STATE_DIR"
echo "  backup(NFS)  = $BACKUP_ROOT/$GALAXY"

# ═══════════════════════════════════════════════════════════════════════════
# 0. PRECONDITIONS — fail loud before mutating anything (invited-guest safety)
# ═══════════════════════════════════════════════════════════════════════════
step "0. Preconditions"

[ -x "$CS" ] || die "cs binary not found or not executable at $CS (unpack the tarball first)"
green "binary present: $CS"

# Static-ness sanity: musl static links have no interpreter. `file`/`ldd` are the
# canonical checks; be tolerant if neither is installed.
if command -v file >/dev/null 2>&1; then
  if file "$CS" | grep -qE 'statically linked|static-pie'; then
    green "binary is statically linked (glibc-independent)"
  else
    warn "binary does not report 'statically linked' — verify it is musl-static"
  fi
fi

# BLAKE3 seal — reproducibility-without-a-container (niel Q5). Compare the
# on-disk binary against the value pinned in MANIFEST.txt at build time.
if [ -f "$MANIFEST" ]; then
  want="$(awk -F'= *' '/^cs_blake3/{print $2}' "$MANIFEST" | tr -d ' ')"
  got="$(b3 "$CS")"
  if [ "$got" = "NO-B3SUM" ]; then
    warn "no b3sum/blake3 available on this host — cannot verify binary seal"
  elif [ -n "$want" ] && [ "$want" = "$got" ]; then
    green "binary BLAKE3 matches MANIFEST ($got)"
  elif [ -n "$want" ]; then
    die "binary BLAKE3 MISMATCH — shipped=$want on-disk=$got (corrupt/tampered transfer)"
  else
    warn "MANIFEST has no cs_blake3 line — skipping seal check"
  fi
else
  warn "no MANIFEST.txt — skipping BLAKE3 seal check"
fi

# /home/tmp must be LOCAL (single-writer ledger correctness). C1 confirmed btrfs;
# re-assert at boot because the design is falsified if this ever changes.
if command -v df >/dev/null 2>&1; then
  fstype="$(df -T "$STATE_ROOT" 2>/dev/null | awk 'NR==2{print $2}')" || fstype=""
  # df -T may fail if STATE_ROOT does not exist yet; probe its /home/tmp parent.
  [ -z "$fstype" ] && fstype="$(df -T /home/tmp 2>/dev/null | awk 'NR==2{print $2}')" || true
  case "$fstype" in
    nfs*|"") warn "state fs type='$fstype' — expected LOCAL (btrfs/ext4/xfs). NFS breaks single-writer!" ;;
    *)       green "state fs is local ($fstype)" ;;
  esac
fi

# noexec check on the state mount (state is data, but /home/tmp also hosts the
# binary in the canonical layout; C1 confirmed no noexec).
if mount 2>/dev/null | grep -E ' /home/tmp ' | grep -q noexec; then
  warn "/home/tmp is mounted noexec — run the binary from NFS home instead, keep STATE on /home/tmp"
fi

# ollama oracle health (D5 — the sole local oracle, native no-hop).
OLLAMA_HOST="${OLLAMA_HOST:-127.0.0.1:11434}"
if command -v curl >/dev/null 2>&1; then
  if curl -fsS --max-time 4 "http://$OLLAMA_HOST/api/tags" >/dev/null 2>&1; then
    green "ollama reachable at $OLLAMA_HOST"
  else
    warn "ollama NOT reachable at $OLLAMA_HOST — the sovereign oracle is down (start it before tackling work)"
  fi
else
  warn "curl absent — cannot health-check ollama"
fi

if [ "$CHECK_ONLY" = 1 ]; then
  step "check-only: preconditions reported, no mutation performed."
  exit 0
fi

# ═══════════════════════════════════════════════════════════════════════════
# 1. LOCAL STATE ROOT — the single-writer-safe live state directory
# ═══════════════════════════════════════════════════════════════════════════
step "1. Local state root"
if [ -d "$STATE_DIR" ]; then
  green "state dir exists: $STATE_DIR"
else
  mkdir -p "$STATE_DIR"
  green "created state dir: $STATE_DIR"
fi

# ═══════════════════════════════════════════════════════════════════════════
# 2. GALAXY CHECKOUT — must be present on NFS $HOME (git = durability of history)
# ═══════════════════════════════════════════════════════════════════════════
step "2. Galaxy checkout"
if [ -d "$REPO/.git" ] || [ -f "$REPO/.git" ]; then
  green "galaxy checkout present: $REPO"
elif [ -d "$REPO/.cosmon" ]; then
  green "galaxy directory present (no .git): $REPO"
else
  die "galaxy checkout not found at $REPO — clone it first (git, on NFS \$HOME), then re-run"
fi

# Ensure a .cosmon/ exists so walk-up discovery has an anchor.
[ -d "$REPO/.cosmon" ] || mkdir -p "$REPO/.cosmon"

# ═══════════════════════════════════════════════════════════════════════════
# 3. FORMULAS + SKILLS + CONFIG — the IP travels as DATA (D1)
#    Symlink the shipped share into the galaxy's .cosmon so `cs` walk-up finds
#    them, WITHOUT copying (single source of truth = the tarball's share/).
# ═══════════════════════════════════════════════════════════════════════════
step "3. Formulas / skills / config"
link_share() {   # link_share <share-subdir> <dest-under-.cosmon>
  local src="$SHARE_DIR/$1" dst="$REPO/.cosmon/$2"
  [ -e "$src" ] || { warn "share/$1 absent in tarball — skipping"; return 0; }
  if [ -L "$dst" ] && [ "$(readlink "$dst")" = "$src" ]; then
    green ".cosmon/$2 already linked → $src"
  elif [ -e "$dst" ] && [ ! -L "$dst" ]; then
    warn ".cosmon/$2 exists and is not our symlink — leaving in place (galaxy owns it)"
  else
    ln -sfn "$src" "$dst"
    green "linked .cosmon/$2 → $src"
  fi
}
link_share formulas formulas
link_share skills   skills
# config.toml: only seed if the galaxy has none (never clobber a live config).
if [ -f "$REPO/.cosmon/config.toml" ]; then
  green ".cosmon/config.toml present (galaxy owns it)"
elif [ -f "$SHARE_DIR/config.toml" ]; then
  cp "$SHARE_DIR/config.toml" "$REPO/.cosmon/config.toml"
  green "seeded .cosmon/config.toml from tarball"
fi

# ═══════════════════════════════════════════════════════════════════════════
# 4. STATE SYMLINK — .cosmon/state → LOCAL state dir (gitignored, ADR-030)
# ═══════════════════════════════════════════════════════════════════════════
step "4. State symlink (.cosmon/state → local NVMe)"
LIVE_STATE="$REPO/.cosmon/state"
if [ -L "$LIVE_STATE" ]; then
  cur="$(readlink "$LIVE_STATE")"
  if [ "$cur" = "$STATE_DIR" ]; then
    green ".cosmon/state already → $STATE_DIR"
  else
    ln -sfn "$STATE_DIR" "$LIVE_STATE"
    green "re-pointed .cosmon/state → $STATE_DIR (was $cur)"
  fi
elif [ -d "$LIVE_STATE" ]; then
  # A real directory already holds state — migrate it once, then symlink.
  warn ".cosmon/state is a real directory — migrating its contents to $STATE_DIR"
  rsync -a "$LIVE_STATE"/ "$STATE_DIR"/ 2>/dev/null || cp -a "$LIVE_STATE"/. "$STATE_DIR"/
  mv "$LIVE_STATE" "$LIVE_STATE.pre-symlink.$(date +%s)"
  ln -sfn "$STATE_DIR" "$LIVE_STATE"
  green "migrated + symlinked .cosmon/state → $STATE_DIR (backup kept alongside)"
else
  ln -sfn "$STATE_DIR" "$LIVE_STATE"
  green "symlinked .cosmon/state → $STATE_DIR"
fi

# Confirm .cosmon/state is gitignored so the symlink never dirties the tree.
if git -C "$REPO" check-ignore -q .cosmon/state 2>/dev/null; then
  green ".cosmon/state is gitignored (ADR-030) — symlink is invisible to git"
else
  warn ".cosmon/state is NOT gitignored — add 'state/' to .cosmon/.gitignore (ADR-030)"
fi

# ═══════════════════════════════════════════════════════════════════════════
# 5. cs init — one-shot, idempotent (skip if a fleet already exists)
# ═══════════════════════════════════════════════════════════════════════════
step "5. cs init"
export PATH="$BIN_DIR:$PATH"
if [ -f "$STATE_DIR/fleet.json" ] || [ -f "$STATE_DIR/fleets/default/fleet.json" ]; then
  green "fleet already initialised (skipping cs init)"
else
  ( cd "$REPO" && "$CS" init ) && green "cs init complete" || warn "cs init returned non-zero (may already be initialised)"
fi

# ═══════════════════════════════════════════════════════════════════════════
# 6. cosmon.env — sourceable PATH + belt-and-suspenders COSMON_STATE_DIR
# ═══════════════════════════════════════════════════════════════════════════
step "6. cosmon.env"
ENV_FILE="$PREFIX/cosmon.env"
cat > "$ENV_FILE" <<EOF
# Source this to put the sovereign cs on PATH.  Generated by provision.sh.
#   source $ENV_FILE
export PATH="$BIN_DIR:\$PATH"
# COSMON_STATE_DIR is a fallback for one-shot cs invocations OUTSIDE a tmux
# worker (the symlink at $REPO/.cosmon/state is the primary, tmux-env-safe path).
export COSMON_STATE_DIR="$STATE_DIR"
EOF
green "wrote $ENV_FILE"

# ═══════════════════════════════════════════════════════════════════════════
# 7. rsync COLD-COPY MIRROR — /home/tmp state → NFS $HOME every ~INTERVAL
#    (/home/tmp is reboot-wipeable; git covers history, this covers live state)
# ═══════════════════════════════════════════════════════════════════════════
if [ "$DO_BACKUP" = 1 ]; then
  step "7. State cold-copy mirror (every ${INTERVAL}s)"
  mkdir -p "$BACKUP_ROOT"
  BACKUP_SH="$SELF/cosmon-state-backup.sh"
  [ -x "$BACKUP_SH" ] || { warn "cosmon-state-backup.sh missing/not executable — skipping mirror"; DO_BACKUP=0; }
fi

if [ "$DO_BACKUP" = 1 ]; then
  installed=0
  # Preferred: systemd --user timer (survives logout with linger; clean lifecycle).
  if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
    UD="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    mkdir -p "$UD"
    cat > "$UD/cosmon-state-backup.service" <<EOF
[Unit]
Description=Cosmon sovereign state cold-copy mirror (LOCAL /home/tmp -> NFS \$HOME)
[Service]
Type=oneshot
ExecStart=$BACKUP_SH --src $STATE_ROOT --dst $BACKUP_ROOT
EOF
    cat > "$UD/cosmon-state-backup.timer" <<EOF
[Unit]
Description=Run cosmon state cold-copy every ${INTERVAL}s
[Timer]
OnBootSec=${INTERVAL}
OnUnitActiveSec=${INTERVAL}
AccuracySec=15s
Persistent=true
[Install]
WantedBy=timers.target
EOF
    systemctl --user daemon-reload 2>/dev/null || true
    if systemctl --user enable --now cosmon-state-backup.timer 2>/dev/null; then
      green "systemd --user timer installed + started (cosmon-state-backup.timer)"
      warn  "for survival across logout: 'loginctl enable-linger $USER' (needs admin if denied)"
      installed=1
    fi
  fi
  # Fallback: guarded nohup loop (portable, no systemd/cron/root needed).
  if [ "$installed" = 0 ]; then
    PIDFILE="$STATE_ROOT/.backup-loop.pid"
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then
      green "backup loop already running (pid $(cat "$PIDFILE"))"
    else
      setsid bash -c '
        echo $$ > "'"$PIDFILE"'"
        while true; do
          "'"$BACKUP_SH"'" --src "'"$STATE_ROOT"'" --dst "'"$BACKUP_ROOT"'" >/dev/null 2>&1 || true
          sleep "'"$INTERVAL"'"
        done
      ' >/dev/null 2>&1 < /dev/null &
      sleep 1
      if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then
        green "started guarded nohup backup loop (pid $(cat "$PIDFILE"), every ${INTERVAL}s)"
      else
        warn "failed to start backup loop — run '$BACKUP_SH --src $STATE_ROOT --dst $BACKUP_ROOT' from cron/tmux"
      fi
    fi
  fi
  # First pass now, so a mirror exists immediately.
  "$BACKUP_SH" --src "$STATE_ROOT" --dst "$BACKUP_ROOT" && green "initial cold-copy done → $BACKUP_ROOT"
else
  step "7. State cold-copy mirror — SKIPPED (--no-backup)"
  warn "live state on /home/tmp is reboot-VOLATILE with no mirror; enable the backup for durability"
fi

# ═══════════════════════════════════════════════════════════════════════════
# 8. SMOKE — prove the binary round-trips against the wired state
# ═══════════════════════════════════════════════════════════════════════════
step "8. Smoke test"
( cd "$REPO" && "$CS" --version ) && green "cs --version ok"
( cd "$REPO" && "$CS" observe --json >/dev/null 2>&1 ) && green "cs observe round-trip ok" \
  || warn "cs observe returned non-zero (empty fleet is fine on a fresh init)"

step "provision complete — sovereign cosmon converged for galaxy '$GALAXY'"
echo   "  next: 'source $ENV_FILE' then run the cosmon runtime in tmux on g5."
