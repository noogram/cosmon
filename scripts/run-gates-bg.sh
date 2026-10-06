#!/usr/bin/env bash
# run-gates-bg.sh — run the project's full gate bundle DETACHED, so no agent
# shell tool's own patience limit can be mistaken for a gate verdict.
#
# noogram/cosmon#112. `just gates` is documented at ~15 minutes cold
# (CLAUDE.md, Verification) and can run longer on a loaded shared machine, but
# an agent's shell tool typically caps a *foreground* command well under that
# (~10 minutes for Claude Code's Bash tool). A worker that runs the bundle in
# the foreground either gets cut off mid-run or wraps it in its own
# `timeout 600 ...` — and then reads that timeout firing as a failed gate,
# when it measured nothing but the tool's own patience. Four molecules
# collapsed this way on 2026-09-28 although their work was correct and every
# scoped test was green.
#
# This script launches the bundle in a new session, detached from the
# caller's controlling terminal and process group, and returns immediately.
# It survives the caller exiting: killing the caller's shell (or its whole
# process group) sends no signal into a different session. The bundle's
# combined log and its exit code land in known files the caller polls with
# cheap, separate checks — never one long blocking call.
#
# There is no `setsid(1)` on macOS (it is a util-linux tool, Linux-only), so
# the detach is done with `python3 os.setsid()` instead — a dependency this
# repo already assumes locally (AGENTS.md §Verification: "this repo assumes
# only the Rust toolchain + python3 locally"). That one process becomes the
# new session leader and runs the gate command as its child, then publishes
# the exit code itself: nothing is left in the CALLER's session or process
# group to die when that caller does.
#
# Usage:
#   scripts/run-gates-bg.sh                 # runs: just gates
#   scripts/run-gates-bg.sh <cmd> [args...] # runs an arbitrary command instead
#
# Output directory (in this order):
#   $COSMON_RUN_GATES_BG_DIR   — test-only override
#   $COSMON_MOL_DIR            — the molecule state directory `cs tackle`
#                                 injects into every worker (always present
#                                 under a real molecule)
#   ./.cosmon-gates-runs       — fallback for a bare checkout with neither set
#
# Files written there:
#   gates.log    — combined stdout/stderr of the run, from the start
#   gates.pid    — pid of the detached session leader. The leader writes it
#                  itself, after os.setsid() has succeeded, and this script
#                  does not return until it exists: "pid file present" means
#                  "already in its own session", never "about to be"
#   gates.exit   — the run's exit code, written ONLY once it finishes (atomic
#                  rename, so a poller never observes a partial write); any
#                  previous gates.exit is removed before the new run starts, so
#                  "file present" always means "this run is done"
#
# A caller waits like this (cheap, non-blocking, re-run as many times as
# needed — never sit in one long blocking call on this):
#   test -f "$dir/gates.exit" && cat "$dir/gates.exit"
set -euo pipefail

dir="${COSMON_RUN_GATES_BG_DIR:-${COSMON_MOL_DIR:-./.cosmon-gates-runs}}"
mkdir -p "$dir"

log="$dir/gates.log"
pidfile="$dir/gates.pid"
exitfile="$dir/gates.exit"

# A stale exit file from a previous run would make a poller believe THIS run
# is already done before it has even started.
rm -f "$exitfile" "$exitfile.tmp" "$pidfile" "$pidfile.tmp"
: > "$log"

if [[ $# -eq 0 ]]; then
    set -- just gates
fi

# The python3 process below calls os.setsid() FIRST — becoming a new session
# leader detached from this script's controlling terminal — and only then
# runs the gate command as its own child, so the child inherits that new
# session too. It captures the child's exit code itself and publishes it via
# atomic rename, all from inside the detached session: there is nothing left
# for `wait` to do out here once this script returns.
detach_py='
import os
import subprocess
import sys

exitfile = sys.argv[1]
pidfile = sys.argv[2]
cmd = sys.argv[3:]

os.setsid()
# Published only now, so the launcher can wait until the detach has really
# happened (see the wait loop in run-gates-bg.sh).
with open(pidfile + ".tmp", "w", encoding="ascii") as f:
    f.write(str(os.getpid()))
os.replace(pidfile + ".tmp", pidfile)
rc = subprocess.call(cmd)

tmp = exitfile + ".tmp"
with open(tmp, "w", encoding="ascii") as f:
    f.write(str(rc))
os.replace(tmp, exitfile)
sys.exit(rc)
'
python3 -c "$detach_py" "$exitfile" "$pidfile" "$@" >"$log" 2>&1 </dev/null &
child=$!
disown "$child" 2>/dev/null || true

# Until os.setsid() has run, the child is still in the caller's session and
# process group. Starting the interpreter takes tens to hundreds of ms (a
# pyenv shim adds more), and a command runner that ends the command's whole
# tree the moment the command returns kills it inside that window: no
# gates.exit is ever written (noogram/cosmon#176, observed under `codex
# exec`). So do not return before the child has published its pid from
# inside the new session. The wait ends early only if the child died
# (`kill -0` fails). The 120 s bound is a backstop for a child that is alive
# but never gets there: interpreter start-up was measured at 3-7 s under a
# load average near 400 (pyenv shim, 16 cores), and the 15 s bound this
# replaces turned a slow start into a false failure. 0.05 s steps.
for _ in $(seq 1 2400); do
    [[ -s "$pidfile" ]] && break
    kill -0 "$child" 2>/dev/null || break
    sleep 0.05
done
if [[ ! -s "$pidfile" ]]; then
    echo "run-gates-bg: detached runner did not start (see $log)" >&2
    exit 1
fi
child="$(cat "$pidfile")"

echo "gates running detached (pid $child)"
echo "log:  $log"
echo "exit: $exitfile (poll for this file; absence = still running)"
