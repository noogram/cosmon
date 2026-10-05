#!/usr/bin/env bash
# Exercise the supervisor unit against a real per-user service manager.
set -euo pipefail

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
unit="$HOME/.config/systemd/user/cosmon-daemon-supervisor.service"
fixture_root="${COSMON_SYSTEMD_TEST_TMPDIR:-${TMPDIR:-/tmp}}/cosmon systemd-%-\$-quote\"-slash\\"
mkdir -p "$fixture_root"
work="$(mktemp -d "$fixture_root/run.XXXXXX")"
original=
TEST_CHILD_PID="$work/child.pid"
TEST_SUPERVISOR_PID="$work/supervisor.pid"

fail() {
    echo "systemd-supervisor-test: $*" >&2
    exit 1
}

cleanup() {
    systemctl --user disable --now cosmon-daemon-supervisor.service >/dev/null 2>&1 || true
    if [[ -n "$original" && -f "$original" ]]; then
        mkdir -p "$(dirname -- "$unit")"
        cp "$original" "$unit"
    else
        rm -f "$unit"
    fi
    systemctl --user daemon-reload >/dev/null 2>&1 || true
    for pid_file in "$TEST_CHILD_PID" "$TEST_SUPERVISOR_PID"; do
        if [[ -s "$pid_file" ]]; then
            kill "$(<"$pid_file")" >/dev/null 2>&1 || true
        fi
    done
    find "$work" -depth -delete
    rmdir "$fixture_root" 2>/dev/null || true
}
trap cleanup EXIT

[[ "$(uname -s)" == Linux ]] || fail "Linux is required"
systemctl --user show-environment >/dev/null 2>&1 || fail "user service manager is unreachable"
if [[ -e "$unit" ]]; then
    original="$work/original.service"
    cp "$unit" "$original"
fi

mkdir -p "$work/bin" "$work/config"
cat > "$work/bin/child" <<'EOF'
#!/bin/sh
trap '' TERM
dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
echo "$$" > "$dir/../child.pid"
while :; do sleep 1; done
EOF
cat > "$work/bin/cosmon-daemon-supervisor" <<'EOF'
#!/bin/sh
if [ "${3:-}" = "--check" ] || [ "${1:-}" = "--check" ]; then
    [ -s "${2:-}" ]
    exit
fi
dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
"$dir/child" &
child=$!
on_term() {
    kill -TERM "$child" 2>/dev/null || true
    wait "$child"
}
trap on_term TERM
echo "$$" > "$dir/../supervisor.pid"
while :; do sleep 1; done
EOF
chmod +x "$work/bin/child" "$work/bin/cosmon-daemon-supervisor"
printf 'fixture=true\n' > "$work/config/daemons.toml"

export COSMON_SUPERVISOR_BIN_DIR="$work/bin"
export COSMON_SUPERVISOR_CONFIG="$work/config/daemons.toml"

"$repo/scripts/install-daemon-supervisor.sh" install
systemctl --user is-enabled --quiet cosmon-daemon-supervisor.service || fail "unit is not enabled"
systemctl --user is-active --quiet cosmon-daemon-supervisor.service || fail "unit is not active"

for _ in {1..50}; do
    [[ -s "$TEST_SUPERVISOR_PID" && -s "$TEST_CHILD_PID" ]] && break
    sleep 0.1
done
[[ -s "$TEST_SUPERVISOR_PID" && -s "$TEST_CHILD_PID" ]] || fail "service PIDs were not recorded"

# Reload is a real-manager replacement, not only a renderer assertion. A
# changed config must be revalidated, the previous cgroup must be cleaned, and
# exactly one replacement must become the service main process.
pre_reload_supervisor="$(<"$TEST_SUPERVISOR_PID")"
pre_reload_child="$(<"$TEST_CHILD_PID")"
printf 'fixture=reloaded\n' > "$work/config/daemons.toml"
"$repo/scripts/install-daemon-supervisor.sh" reload
for _ in {1..50}; do
    reload_supervisor="$(cat "$TEST_SUPERVISOR_PID" 2>/dev/null || true)"
    reload_child="$(cat "$TEST_CHILD_PID" 2>/dev/null || true)"
    [[ -n "$reload_supervisor" && "$reload_supervisor" != "$pre_reload_supervisor" ]] && break
    sleep 0.1
done
[[ "$reload_supervisor" != "$pre_reload_supervisor" ]] || fail "reload did not replace the supervisor"
kill -0 "$pre_reload_supervisor" 2>/dev/null && fail "pre-reload supervisor survived replacement"
kill -0 "$pre_reload_child" 2>/dev/null && fail "pre-reload descendant survived cgroup cleanup"
kill -0 "$reload_supervisor" 2>/dev/null || fail "reloaded supervisor is absent"
kill -0 "$reload_child" 2>/dev/null || fail "reloaded child is absent"

old_supervisor="$(<"$TEST_SUPERVISOR_PID")"
old_child="$(<"$TEST_CHILD_PID")"
kill -KILL "$old_supervisor"
for _ in {1..120}; do
    new_supervisor="$(cat "$TEST_SUPERVISOR_PID" 2>/dev/null || true)"
    new_child="$(cat "$TEST_CHILD_PID" 2>/dev/null || true)"
    [[ -n "$new_supervisor" && "$new_supervisor" != "$old_supervisor" ]] && break
    sleep 0.1
done
[[ "$new_supervisor" != "$old_supervisor" ]] || fail "supervisor was not replaced"
kill -0 "$old_child" 2>/dev/null && fail "old descendant survived crash replacement"
kill -0 "$new_supervisor" 2>/dev/null || fail "replacement supervisor is absent"
kill -0 "$new_child" 2>/dev/null || fail "replacement child is absent"

systemctl --user show cosmon-daemon-supervisor.service \
    -p MainPID -p ActiveState -p SubState -p Restart -p RestartUSec \
    -p KillMode -p TimeoutStopUSec -p FragmentPath --no-pager
printf 'observed old supervisor PID=%s old child PID=%s replacement supervisor PID=%s replacement child PID=%s\n' \
    "$old_supervisor" "$old_child" "$new_supervisor" "$new_child"

systemctl --user stop cosmon-daemon-supervisor.service
systemctl --user is-active --quiet cosmon-daemon-supervisor.service && fail "unit remained active after stop"
kill -0 "$new_child" 2>/dev/null && fail "TERM-ignoring descendant survived final cgroup cleanup"

"$repo/scripts/install-daemon-supervisor.sh" uninstall
[[ ! -e "$unit" ]] || fail "owned unit remains after uninstall"
[[ -f "$work/config/daemons.toml" ]] || fail "uninstall removed application config"
echo "systemd-supervisor-test: PASS"
