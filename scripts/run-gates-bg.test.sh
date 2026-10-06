#!/usr/bin/env bash
# Exercise scripts/run-gates-bg.sh (noogram/cosmon#112).
#
# The property under test is a DETACH, not just "it runs a command in the
# background": a background job in the caller's own process group dies when
# that group is killed, which is exactly the shape an agent's shell tool
# produces when its own foreground timeout fires. This test kills the
# LAUNCHER's entire process group immediately after launch and asserts the
# gate command still runs to completion and publishes its exit code — the
# thing four molecules needed on 2026-09-28 and did not have.
#
# Hermetic: temp directories only, no real `just gates` is ever run here.

set -uo pipefail
cd "$(dirname "$0")/.."
SCRIPT="$PWD/scripts/run-gates-bg.sh"

pass=0; fail=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; pass=$((pass+1)); }
ko()  { printf '  \033[31m✗\033[0m %s — %s\n' "$1" "$2"; fail=$((fail+1)); }

tmp="$(mktemp -d -t run-gates-bg-test-XXXXXX)"
trap 'rm -rf "$tmp"' EXIT

echo "── run-gates-bg.sh ───────────────────────────────────────────────────"

# ── 1. returns immediately instead of blocking for the command's duration ──
dir1="$tmp/immediate"; mkdir -p "$dir1"
# The command outlasts any plausible launcher wait (interpreter start-up alone
# takes seconds under load), so the property is "the launcher returned while
# the command was still running", not a wall-clock threshold.
COSMON_RUN_GATES_BG_DIR="$dir1" "$SCRIPT" sleep 120 >/dev/null
[ ! -f "$dir1/gates.exit" ] \
    && ok "launch returns before the command finishes" \
    || ko "launch returns before the command finishes" "gates.exit already present"
[ -f "$dir1/gates.pid" ] \
    && ok "pid file is written before the launcher returns" \
    || ko "pid file is written before the launcher returns" "missing"
# drain: end the long sleep (the pid is the session leader; its group holds it)
kill -TERM -- "-$(cat "$dir1/gates.pid")" 2>/dev/null || true
for _ in $(seq 1 100); do [ -f "$dir1/gates.exit" ] && break; sleep 0.1; done

# ── 2. survives the LAUNCHER's entire process group being killed ───────────
#
# The launcher itself runs inside a throwaway session (there is no
# `setsid(1)` on macOS, so this uses the same `python3 os.setsid()` trick as
# the script under test) so we can kill -KILL its whole process group without
# taking down this test script's own shell. If run-gates-bg.sh's detach were
# only `cmd &` (no new session), the detached job would sit in that same
# group and die with it.
dir2="$tmp/survives-kill"; mkdir -p "$dir2"
python3 -c '
import os, subprocess, sys
os.setsid()
subprocess.call(["bash", "-c", sys.argv[1]])
' "
    COSMON_RUN_GATES_BG_DIR='$dir2' '$SCRIPT' sleep 5
    sleep 60
" &
launcher_pid=$!
# give run-gates-bg.sh time to fork its own detached child and return
for _ in $(seq 1 100); do [ -f "$dir2/gates.pid" ] && break; sleep 0.05; done
launcher_pgid=$(ps -o pgid= -p "$launcher_pid" 2>/dev/null | tr -d ' ')
gates_pid=$(cat "$dir2/gates.pid" 2>/dev/null || echo "")
# `ps -o sess=` prints 0 for every process without a controlling tty on
# macOS — useless here. `os.getsid()` reads the real session id.
getsid() { python3 -c 'import os,sys; print(os.getsid(int(sys.argv[1])))' "$1" 2>/dev/null || echo ""; }
gates_sid=$(getsid "$gates_pid")
launcher_sid=$(getsid "$launcher_pid")
[ -n "$gates_sid" ] && [ -n "$launcher_sid" ] && [ "$gates_sid" != "$launcher_sid" ] \
    && ok "the detached run sits in a different session than the launcher" \
    || ko "detached run is in a different session" "gates_sid=$gates_sid launcher_sid=$launcher_sid"
if [ -n "$launcher_pgid" ]; then
    kill -KILL -- "-$launcher_pgid" 2>/dev/null || true
fi
wait "$launcher_pid" 2>/dev/null
for _ in $(seq 1 100); do [ -f "$dir2/gates.exit" ] && break; sleep 0.1; done
[ -f "$dir2/gates.exit" ] \
    && ok "the exit-code file is written after the launcher's group was killed" \
    || ko "exit-code file appears after launcher group killed" "never appeared"
[ "$(cat "$dir2/gates.exit" 2>/dev/null)" = "0" ] \
    && ok "the survived run's success is recorded (exit 0)" \
    || ko "survived run recorded success" "got $(cat "$dir2/gates.exit" 2>/dev/null)"

# ── 3. exit code file is written for a FAILING command too ─────────────────
dir3="$tmp/fails"; mkdir -p "$dir3"
COSMON_RUN_GATES_BG_DIR="$dir3" "$SCRIPT" bash -c 'echo boom >&2; exit 7' >/dev/null
for _ in $(seq 1 20); do [ -f "$dir3/gates.exit" ] && break; sleep 0.1; done
[ "$(cat "$dir3/gates.exit" 2>/dev/null)" = "7" ] \
    && ok "a failing command's exit code (7) is recorded, not swallowed" \
    || ko "failing command's exit code is recorded" "got $(cat "$dir3/gates.exit" 2>/dev/null)"
grep -q "boom" "$dir3/gates.log" 2>/dev/null \
    && ok "the command's output lands in gates.log" \
    || ko "command output lands in gates.log" "not found"

# ── 4. a stale exit file from a previous run does not lie about a new one ──
dir4="$tmp/stale"; mkdir -p "$dir4"
echo "99" > "$dir4/gates.exit"
COSMON_RUN_GATES_BG_DIR="$dir4" "$SCRIPT" sleep 0.3 >/dev/null
[ ! -f "$dir4/gates.exit" ] \
    && ok "launching a new run clears the previous exit file at once" \
    || ko "new run clears the previous exit file" "stale value still present"
for _ in $(seq 1 20); do [ -f "$dir4/gates.exit" ] && break; sleep 0.1; done
[ "$(cat "$dir4/gates.exit" 2>/dev/null)" = "0" ] \
    && ok "the new run's own exit code replaces the stale one" \
    || ko "new run's exit code replaces stale one" "got $(cat "$dir4/gates.exit" 2>/dev/null)"

# ── 5. survives a runner that kills the launcher's tree the instant it returns ─
#
# noogram/cosmon#176: under `codex exec` the launcher returned in ~200 ms and
# the detached run never produced gates.exit. The child only leaves the
# caller's session when its interpreter reaches os.setsid(); a slow interpreter
# start (a pyenv shim, a cold disk) leaves a window after the launcher returns
# in which the child is still in the caller's group. Here a `python3` shim
# makes that window 1 s wide, and the "runner" kills its own process group
# right after the launcher returns. The property: the run still completes and
# publishes its exit code.
dir5="$tmp/instant-kill"; mkdir -p "$dir5" "$tmp/slowpy"
real_py="$(command -v python3)"
printf '#!/bin/sh\nsleep 1\nexec "%s" "$@"\n' "$real_py" > "$tmp/slowpy/python3"
chmod +x "$tmp/slowpy/python3"
( "$real_py" -c '
import os, subprocess, sys
os.setsid()
subprocess.call(["bash", "-c", sys.argv[1]])
' "
    PATH='$tmp/slowpy':\$PATH COSMON_RUN_GATES_BG_DIR='$dir5' '$SCRIPT' sleep 1 >/dev/null
    kill -KILL 0
" ) >/dev/null 2>&1
for _ in $(seq 1 100); do [ -f "$dir5/gates.exit" ] && break; sleep 0.1; done
[ "$(cat "$dir5/gates.exit" 2>/dev/null)" = "0" ] \
    && ok "the run completes although the runner killed the launcher's tree on return" \
    || ko "run survives an instant kill of the launcher's tree" "gates.exit: '$(cat "$dir5/gates.exit" 2>/dev/null)'"

echo "──────────────────────────────────────────────────────────────────────"
if [ "$fail" -eq 0 ]; then
    echo "run-gates-bg.test: $pass passed, 0 failed."
    exit 0
fi
echo "run-gates-bg.test: $pass passed, $fail FAILED." >&2
exit 1
