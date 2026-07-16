#!/usr/bin/env bash
# reboot-survival-check.sh — promote "the runtime survives reboots" from a
# claim to a check (task-20260608-1c59, kahneman).
#
# The Resident Runtime has NO plist of its own (two restart authorities for
# one process = double-spawn). It survives reboots transitively: launchd
# keeps the supervisor alive (KeepAlive + RunAtLoad), the supervisor reads
# ~/.config/cosmon/daemons.toml and keeps every `enabled = true` child alive
# — runtime + scheduler + bots included. This script asserts the *static*
# preconditions and the *live* end-state without rebooting, and prints the
# manual reboot soak procedure for the real thing.
#
# Exit codes:
#   0 — every assertion held
#   1 — an assertion failed (the tree would NOT re-establish on reboot)
#   2 — usage / environment error
#
# Usage:
#   scripts/soak/reboot-survival-check.sh          # run the static + live checks
#   scripts/soak/reboot-survival-check.sh --soak   # print the manual reboot procedure
set -euo pipefail

LABEL="com.cosmon.daemon-supervisor"
HOME_DIR="${HOME:?HOME must be set}"
REPO_PLIST="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/launchd/${LABEL}.plist"
INSTALLED_PLIST="${HOME_DIR}/Library/LaunchAgents/${LABEL}.plist"
DAEMONS_TOML="${HOME_DIR}/.config/cosmon/daemons.toml"

fail=0
say()  { printf '%s\n' "$*"; }
ok()   { printf '  \033[32mok\033[0m   %s\n' "$*"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$*"; fail=1; }
warn() { printf '  \033[33mwarn\033[0m %s\n' "$*"; }

if [[ "${1:-}" == "--soak" ]]; then
    cat <<'PROC'
Manual reboot soak (the real assertion — operator-gated):

  1. Ensure the runtime is enabled:
       grep -A1 'name = "cosmon-runtime"' ~/.config/cosmon/daemons.toml
       # ...enabled = true
  2. Record the live tree BEFORE:
       launchctl list | grep cosmon
       cs daemons list
  3. Reboot the machine.
  4. After login, WAIT ~60s for launchd RunAtLoad + supervisor debounce, then:
       scripts/soak/reboot-survival-check.sh
  5. PASS iff: supervisor is back (launchd RunAtLoad), and every
     `enabled = true` daemon — runtime, scheduler, bots — has a fresh pid.
     The runtime trace should resume:
       tail -n 5 /srv/cosmon/cosmon/.cosmon/state/runtime-trace.jsonl

The point: nobody owns a runtime plist. If the supervisor comes back and
re-spawns its children, the runtime is back too — by inheritance, not by a
second restart authority.
PROC
    exit 0
fi

# ---------------------------------------------------------------------------
# 1. Static preconditions — the supervisor plist carries the reboot anchors.
# ---------------------------------------------------------------------------
say "1. supervisor plist anchors (the single restart authority)"
PLIST_TO_CHECK="$REPO_PLIST"
[[ -f "$INSTALLED_PLIST" ]] && PLIST_TO_CHECK="$INSTALLED_PLIST"
if [[ ! -f "$PLIST_TO_CHECK" ]]; then
    bad "no supervisor plist found (looked at $REPO_PLIST and $INSTALLED_PLIST)"
else
    say "  (reading $PLIST_TO_CHECK)"
    # KeepAlive = true  → launchd respawns the supervisor if it dies.
    if grep -A1 -E '<key>KeepAlive</key>' "$PLIST_TO_CHECK" | grep -q '<true/>'; then
        ok "KeepAlive = true"
    else
        bad "KeepAlive is not true — a crashed supervisor would stay dead"
    fi
    # RunAtLoad = true  → launchd starts the supervisor at boot/login.
    if grep -A1 -E '<key>RunAtLoad</key>' "$PLIST_TO_CHECK" | grep -q '<true/>'; then
        ok "RunAtLoad = true"
    else
        bad "RunAtLoad is not true — the supervisor would not start on reboot"
    fi
fi

# ---------------------------------------------------------------------------
# 2. No competing restart authority — the runtime must NOT have its own plist.
# ---------------------------------------------------------------------------
say "2. no double-spawn (runtime has no plist of its own)"
rogue=0
for d in "$HOME_DIR/Library/LaunchAgents" /Library/LaunchAgents /Library/LaunchDaemons; do
    [[ -d "$d" ]] || continue
    while IFS= read -r p; do
        [[ -z "$p" ]] && continue
        rogue=1
        bad "found a runtime-owned plist: $p (delete it — supervisor owns the runtime)"
    done < <(grep -rl -E 'cosmon-runtime|cs.*run.*--resident' "$d" 2>/dev/null || true)
done
[[ "$rogue" -eq 0 ]] && ok "no standalone runtime LaunchAgent (good — single authority)"

# ---------------------------------------------------------------------------
# 3. Live end-state — the tree a reboot must re-establish is up right now.
# ---------------------------------------------------------------------------
say "3. live tree (what a reboot must reproduce)"
if launchctl list 2>/dev/null | awk -v lbl="$LABEL" '$3 == lbl { f=1 } END { exit !f }'; then
    ok "supervisor loaded in launchd ($LABEL)"
else
    warn "supervisor not loaded in launchd — run scripts/install-daemon-supervisor.sh install"
fi

if command -v pgrep >/dev/null 2>&1; then
    if pgrep -f cosmon-daemon-supervisor >/dev/null 2>&1; then
        ok "supervisor process alive"
    else
        warn "supervisor process not found (not installed yet?)"
    fi
fi

if [[ -f "$DAEMONS_TOML" ]]; then
    ok "daemons.toml present ($DAEMONS_TOML)"
    if command -v cs >/dev/null 2>&1; then
        say "  cs daemons list:"
        cs daemons list 2>/dev/null | sed 's/^/    /' || warn "cs daemons list failed"
    else
        warn "cs not on PATH — cannot enumerate live daemons"
    fi
else
    warn "no daemons.toml at $DAEMONS_TOML — nothing to supervise yet"
fi

say ""
if [[ "$fail" -eq 0 ]]; then
    say "PASS — reboot-survival preconditions hold. For the real assertion run:"
    say "  scripts/soak/reboot-survival-check.sh --soak"
    exit 0
else
    say "FAIL — at least one reboot-survival precondition is broken (see above)."
    exit 1
fi
