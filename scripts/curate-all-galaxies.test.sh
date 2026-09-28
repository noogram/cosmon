#!/usr/bin/env bash
# curate-all-galaxies.test.sh — kill-switch regression tests for
# `scripts/curate-all-galaxies.sh` (issue #108).
#
# The sweep must not nucleate anything when either the global
# `~/.cosmon/stand-down.lock` or the scoped `~/.cosmon/autopilot.off` is
# present. `HOME` points at a tempdir holding one fake galaxy, and a fake
# `cs` on PATH records every call, so a nucleation attempt is observable
# without touching a real galaxy. A control case with no switch asserts the
# fixture does reach `cs nucleate`; without it, "not called" could pass for
# a fixture that never gets that far.
#
# Usage: scripts/curate-all-galaxies.test.sh
# Exit codes: 0 all cases pass, 1 at least one failed.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SWEEP="${CURATE_SWEEP:-${SCRIPT_DIR}/curate-all-galaxies.sh}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/curate-all-galaxies-test.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

FAILS=0

# run_case <name> <switch-file-or-empty> <expect: called|not-called>
run_case() {
    local name="$1" switch="$2" expect="$3"
    local home="$TMP_DIR/$name/home"
    local bin="$TMP_DIR/$name/bin"
    local calls="$TMP_DIR/$name/cs-calls"
    mkdir -p "$home/.cosmon" "$home/.config/cosmon" "$home/galaxies/demo/.cosmon" "$bin"
    printf '[drain]\ngalaxies = ["demo"]\n' > "$home/.config/cosmon/curate.toml"
    # Fake cs: record the call and fail, so the sweep logs FAIL and moves on.
    cat > "$bin/cs" <<SH
#!/bin/sh
echo "\$@" >> "$calls"
exit 1
SH
    chmod +x "$bin/cs"
    if [[ -n "$switch" ]]; then
        : > "$home/.cosmon/$switch"
    fi

    env -u COSMON_AUTOPILOT_KILL_SWITCH -u COSMON_CURATE_CONFIG \
        HOME="$home" PATH="$bin:$PATH" bash "$SWEEP" || true

    local got="not-called"
    [[ -s "$calls" ]] && got="called"
    if [[ "$got" == "$expect" ]]; then
        echo "ok   $name ($got)"
    else
        echo "FAIL $name: expected $expect, got $got" >&2
        sed 's/^/     log: /' "$home/.cosmon/curate.log" >&2 || true
        FAILS=$((FAILS + 1))
    fi
}

run_case control          ""                 called
run_case stand-down       stand-down.lock    not-called
run_case autopilot-off    autopilot.off      not-called

if [[ "$FAILS" -gt 0 ]]; then
    echo "$FAILS case(s) failed" >&2
    exit 1
fi
