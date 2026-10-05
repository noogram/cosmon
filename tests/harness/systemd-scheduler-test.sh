#!/usr/bin/env bash
# Exercise the scheduler units against a real per-user service manager.
set -euo pipefail

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
unit_dir="$HOME/.config/systemd/user"
service="$unit_dir/cosmon-scheduler.service"
timer="$unit_dir/cosmon-scheduler.timer"
negative="$unit_dir/cosmon-scheduler-negative.service"
unrelated="$unit_dir/cosmon-w2-unrelated.service"
work="$(mktemp -d "${COSMON_SYSTEMD_TEST_TMPDIR:-${TMPDIR:-/tmp}}/cosmon-w2-green.XXXXXX")"
original_service=
original_timer=
child_pids="$work/child.pids"

fail() {
    echo "systemd-scheduler-test: $*" >&2
    exit 1
}

cleanup() {
    systemctl --user disable --now cosmon-scheduler.timer >/dev/null 2>&1 || true
    systemctl --user stop cosmon-scheduler.service cosmon-scheduler-negative.service \
        >/dev/null 2>&1 || true
    rm -f "$negative"
    rm -f "$unrelated"
    if [[ -n "$original_service" && -f "$original_service" ]]; then
        cp "$original_service" "$service"
    else
        rm -f "$service"
    fi
    if [[ -n "$original_timer" && -f "$original_timer" ]]; then
        cp "$original_timer" "$timer"
    else
        rm -f "$timer"
    fi
    systemctl --user daemon-reload >/dev/null 2>&1 || true
    if [[ -f "$child_pids" ]]; then
        while read -r pid; do kill "$pid" >/dev/null 2>&1 || true; done < "$child_pids"
    fi
    find "$work" -depth -delete
}
trap cleanup EXIT

[[ "$(uname -s)" == Linux ]] || fail "Linux is required"
systemctl --user show-environment >/dev/null 2>&1 || fail "user service manager is unreachable"
mkdir -p "$unit_dir"
if [[ -e "$service" ]]; then original_service="$work/original.service"; cp "$service" "$original_service"; fi
if [[ -e "$timer" ]]; then original_timer="$work/original.timer"; cp "$timer" "$original_timer"; fi

mkdir -p "$work/bin" "$work/config"
cat > "$work/bin/detached-child" <<'EOF'
#!/bin/sh
dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
printf '%s\n' "$$" >> "$dir/child.pids"
sleep "$(cat "$dir/delay")"
printf '%s pid=%s\n' "$(date --iso-8601=ns)" "$$" >> "$dir/markers"
EOF
cat > "$work/bin/cosmon-scheduler" <<'EOF'
#!/bin/sh
dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
if [ "${1:-}" = validate ]; then
    [ "${2:-}" = --config ] && [ -s "${3:-}" ]
    exit
fi
[ "${1:-}" = tick ] || exit 2
if ! mkdir "$dir/main.lock" 2>/dev/null; then
    printf '%s pid=%s\n' "$(date --iso-8601=ns)" "$$" >> "$dir/main-overlap"
fi
trap 'rmdir "$dir/main.lock" 2>/dev/null || true' EXIT
printf '%s pid=%s\n' "$(date --iso-8601=ns)" "$$" >> "$dir/ticks"
case "$(cat "$dir/mode")" in
    detached) "$dir/bin/detached-child" & ;;
    slow) sleep 3 ;;
esac
EOF
cat > "$work/bin/negative" <<'EOF'
#!/bin/sh
dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
(sleep 1; printf 'unexpected\n' >> "$dir/negative-marker") &
exit 0
EOF
chmod +x "$work/bin/detached-child" "$work/bin/cosmon-scheduler" "$work/bin/negative"
printf 'fixture=true\n' > "$work/config/patrols.toml"
printf 'detached\n' > "$work/mode"
printf '2\n' > "$work/delay"
printf 'retain\n' > "$work/scheduler.state.json"
printf 'retain\n' > "$unrelated"
: > "$work/ticks"
: > "$work/markers"

export COSMON_SCHEDULER_BIN_DIR="$work/bin"
export COSMON_SCHEDULER_CONFIG="$work/config/patrols.toml"
"$repo/scripts/install-scheduler.sh" install
systemctl --user is-enabled --quiet cosmon-scheduler.timer || fail "timer is not enabled"
systemctl --user is-active --quiet cosmon-scheduler.timer || fail "timer is not active"

# Exercise the installed reload path against the real manager before replacing
# the timer cadence for the bounded harness run.
printf 'fixture=reloaded\n' > "$work/config/patrols.toml"
"$repo/scripts/install-scheduler.sh" reload
systemctl --user is-enabled --quiet cosmon-scheduler.timer || fail "timer is not enabled after reload"
systemctl --user is-active --quiet cosmon-scheduler.timer || fail "timer is not active after reload"

systemctl --user show cosmon-scheduler.service -p Type -p KillMode -p FragmentPath --no-pager
systemctl --user show cosmon-scheduler.timer -p ActiveState -p SubState -p UnitFileState \
    -p TimersMonotonic -p AccuracyUSec -p RandomizedDelayUSec -p Persistent \
    -p FragmentPath --no-pager

# Deliberate negative control: default control-group cleanup removes a child
# left behind by a completed one-shot main process.
cat > "$negative" <<EOF
[Unit]
Description=Cosmon scheduler cleanup negative control
[Service]
Type=oneshot
ExecStart=$work/bin/negative
KillMode=control-group
EOF
systemctl --user daemon-reload
systemctl --user start cosmon-scheduler-negative.service
sleep 2
[[ ! -e "$work/negative-marker" ]] || fail "negative-control child escaped cgroup cleanup"

# Speed up only the harness clock after recording the installed 60-second
# properties. The service definition under test is unchanged.
sed -e 's/60s/1s/g' -e 's/AccuracySec=1s/AccuracySec=100ms/' \
    "$repo/scripts/systemd/cosmon-scheduler.timer" > "$timer"
systemctl --user daemon-reload
systemctl --user restart cosmon-scheduler.timer
for _ in {1..150}; do
    [[ "$(wc -l < "$work/ticks" 2>/dev/null || echo 0)" -ge 3 ]] && break
    sleep 0.1
done
tick_count="$(wc -l < "$work/ticks" 2>/dev/null || echo 0)"
[[ "$tick_count" -ge 3 ]] || fail "fewer than three timer firings were observed"
sleep 3
marker_count="$(wc -l < "$work/markers" 2>/dev/null || echo 0)"
[[ "$marker_count" -ge 3 ]] || fail "detached children did not survive tick exit"
[[ ! -e "$work/main-overlap" ]] || fail "scheduler main processes overlapped"
echo "timer firings:"
sed -n '1,6p' "$work/ticks"
echo "detached completions:"
sed -n '1,6p' "$work/markers"

# A slow wait-mode tick remains the active oneshot, so later timer deadlines
# cannot create a simultaneous second scheduler main process.
systemctl --user stop cosmon-scheduler.timer
: > "$work/ticks"
rm -f "$work/main-overlap"
printf 'slow\n' > "$work/mode"
systemctl --user start cosmon-scheduler.timer
sleep 5
[[ ! -e "$work/main-overlap" ]] || fail "slow wait-mode ticks overlapped"
slow_count="$(wc -l < "$work/ticks" 2>/dev/null || echo 0)"
[[ "$slow_count" -le 2 ]] || fail "slow wait-mode patrol did not suppress timer activations"

# Uninstall prevents future ticks but does not retroactively cancel a detached
# child. State, config, and unrelated units remain operator-owned.
systemctl --user stop cosmon-scheduler.timer
printf 'detached\n' > "$work/mode"
printf '3\n' > "$work/delay"
before_markers="$(wc -l < "$work/markers" 2>/dev/null || echo 0)"
systemctl --user start cosmon-scheduler.service
before_ticks="$(wc -l < "$work/ticks")"
"$repo/scripts/install-scheduler.sh" uninstall
[[ ! -e "$service" && ! -e "$timer" ]] || fail "owned units remain after uninstall"
sleep 4
after_ticks="$(wc -l < "$work/ticks")"
after_markers="$(wc -l < "$work/markers")"
[[ "$after_ticks" == "$before_ticks" ]] || fail "a tick fired after uninstall"
[[ "$after_markers" -gt "$before_markers" ]] || fail "detached child was cancelled by uninstall"
[[ -f "$work/config/patrols.toml" && -f "$work/scheduler.state.json" ]] || fail "scheduler data was removed"
[[ -f "$unrelated" ]] || fail "unrelated unit was removed"

printf 'observed tick PIDs and timestamps:\n'
cat "$work/ticks"
printf 'observed detached child PIDs and timestamps:\n'
cat "$work/markers"
echo "systemd-scheduler-test: PASS"
