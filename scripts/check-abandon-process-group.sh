#!/usr/bin/env bash
# check-abandon-process-group.sh — list installed one-shot LaunchAgents that
# do NOT set AbandonProcessGroup, so an operator can decide which ones need
# it.
#
# ## The failure mode this exists for
#
# launchd SIGKILLs a job's entire process group the instant the job's main
# process exits. For a one-shot job — StartInterval or StartCalendarInterval,
# no KeepAlive — that instant comes almost immediately. Any work the job
# dispatched as a detached child is therefore killed before it produces
# anything, and the parent has already logged that it fired. `nohup` and
# `trap '' HUP` do not help: the signal is addressed to the group, not to
# the process.
#
# The observed shape (cosmon-scheduler, one 60-second patrol over a 48-hour
# window): 7276 fires recorded by the scheduler, 114 starts reaching the
# patrol's own log, a handful of complete runs — only the ticks that
# happened to last long enough. The index that patrol feeds sat frozen about
# 18 hours and no log said so. For an attention tool, silence is
# indistinguishable from health, which makes this the worst failure mode
# available.
#
# ## What this script can and cannot decide
#
# It decides the SHAPE — one-shot, key absent — which is decidable from the
# plist alone. It CANNOT decide whether a given job dispatches detached
# work; that lives in the program it runs. So every line below is a
# CANDIDATE for review, not a defect. A one-shot job that does all its work
# synchronously and exits is correct without the key, and setting it anyway
# is harmless but pointless.
#
# Read-only. It never edits a plist and never talks to launchctl.
#
# Usage:
#   scripts/check-abandon-process-group.sh                 # the standard agent dirs
#   scripts/check-abandon-process-group.sh DIR [DIR...]    # scan the given dirs
#
# Exit codes:
#   0 — no candidates found
#   1 — operator error (bad args, missing plutil)
#   2 — at least one candidate reported (advisory, for CI/patrol use)

set -euo pipefail

command -v plutil > /dev/null 2>&1 || {
    echo "check-abandon-process-group: plutil not found (macOS only)" >&2
    exit 1
}

usage() {
    sed -n '2,43p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

case "${1:-}" in
    -h | --help | help) usage 0 ;;
esac

if [[ $# -gt 0 ]]; then
    DIRS=("$@")
else
    DIRS=("${HOME}/Library/LaunchAgents" "/Library/LaunchAgents")
fi

# Read one key out of a plist as raw text. Absent key / unreadable plist →
# empty string, which every caller treats as "not set".
plist_key() {
    /usr/bin/plutil -extract "$2" raw -o - -- "$1" 2> /dev/null || true
}

candidates=0
scanned=0

for dir in "${DIRS[@]}"; do
    [[ -d "$dir" ]] || continue
    for plist in "$dir"/*.plist; do
        [[ -e "$plist" ]] || continue
        # A malformed plist is reported, not skipped silently: an agent we
        # cannot read is an agent we cannot clear.
        if ! /usr/bin/plutil -lint -- "$plist" > /dev/null 2>&1; then
            echo "?? unreadable   $plist"
            continue
        fi
        scanned=$((scanned + 1))

        keepalive="$(plist_key "$plist" KeepAlive)"
        interval="$(plist_key "$plist" StartInterval)"
        calendar="$(plist_key "$plist" StartCalendarInterval)"
        abandon="$(plist_key "$plist" AbandonProcessGroup)"
        label="$(plist_key "$plist" Label)"
        [[ -n "$label" ]] || label="$(basename "$plist" .plist)"

        # One-shot = has a clock, and is not kept alive. `KeepAlive` as a
        # dict (conditional restart) still means launchd may restart the
        # job, so it is not the spawn-and-exit shape this is about.
        [[ -n "$interval" || -n "$calendar" ]] || continue
        [[ -z "$keepalive" ]] || continue

        [[ "$abandon" == "true" ]] && continue

        cadence="${interval:+StartInterval=${interval}s}"
        cadence="${cadence:-StartCalendarInterval}"
        echo "!! candidate    ${label}  (${cadence})"
        echo "                ${plist}"
        candidates=$((candidates + 1))
    done
done

echo
echo "scanned ${scanned} readable plists in: ${DIRS[*]}"
if [[ $candidates -eq 0 ]]; then
    echo "no one-shot LaunchAgent is missing AbandonProcessGroup."
    exit 0
fi

echo "${candidates} candidate(s) missing AbandonProcessGroup."
echo
echo "For each: does the job dispatch work meant to outlive the job itself"
echo "(a detached child, a backgrounded command, a spawn-and-forget worker)?"
echo "  yes -> add to its plist, then bootout + bootstrap the agent:"
echo "           <key>AbandonProcessGroup</key>"
echo "           <true/>"
echo "  no  -> nothing to do; it does its work synchronously and exits."
exit 2
