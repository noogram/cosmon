#!/usr/bin/env bash
# Exercise cross-boundary WSL2 evidence with a fake Linux host.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
verify_script="${COSMON_WSL2_VERIFY_SCRIPT:-$script_dir/verify-wsl2-host.sh}"
work="$(mktemp -d)"
home="$work/home"
tools="$work/tools"
state="$work/state"
run_id=test-run
run_root="$state/runs/$run_id"
mkdir -p "$home/.config/cosmon" "$tools" "$run_root/checkpoints" "$run_root/probes"
probe_pid=""
trap '[[ -z "$probe_pid" ]] || kill "$probe_pid" 2>/dev/null || true; rm -rf "$work"' EXIT

cat >"$tools/uname" <<'EOF'
#!/bin/sh
echo Linux
EOF
cat >"$tools/date" <<'EOF'
#!/bin/sh
case "${1:-}" in
  --iso-8601=seconds|--iso-8601=ns) /bin/date -u '+%Y-%m-%dT%H:%M:%SZ' ;;
  *) /bin/date "$@" ;;
esac
EOF
cat >"$tools/uptime" <<'EOF'
#!/bin/sh
[ "$1" = -s ] || exit 2
printf '%s\n' "$MOCK_DISTRIBUTION_BOOT"
EOF
cat >"$tools/powershell.exe" <<'EOF'
#!/bin/sh
printf '%s\n' "$MOCK_WINDOWS_BOOT_EPOCH"
EOF
cat >"$tools/systemctl" <<'EOF'
#!/bin/sh
case "$*" in
  '--user show-environment') exit 0 ;;
  '--user is-active --quiet '*) exit 0 ;;
  '--user show '*)
    case "$*" in
      *' -p MainPID --value') echo 1234 ;;
      *) printf 'LoadState=loaded\nActiveState=active\nSubState=running\n' ;;
    esac
    exit 0
    ;;
  '--user start cosmon-scheduler.service')
    printf 'started\n' >"$MOCK_RUN_ROOT/probes/sleep-started"
    exit 0
    ;;
  *) exit 0 ;;
esac
EOF
cat >"$tools/pgrep" <<'EOF'
#!/bin/sh
echo 1
EOF
cat >"$tools/stat" <<'EOF'
#!/bin/sh
date +%s
EOF
cat >"$tools/sleep" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$tools"/*

/bin/sleep 300 &
probe_pid=$!
printf '%s\n' "$probe_pid" >"$run_root/probes/child.pid"
date +%s >"$run_root/probes/heartbeat"
printf '{}\n' >"$home/.cosmon-state.tmp"
mkdir -p "$home/.cosmon"
mkdir -p "$home/.local/libexec/cosmon"
cat >"$home/.local/libexec/cosmon/install-scheduler.sh" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$home/.local/libexec/cosmon/install-scheduler.sh"
printf '[scheduler]\n' >"$home/.config/cosmon/patrols.toml"
printf '{}\n' >"$home/.cosmon/daemon-supervisor.state.json"
printf '{}\n' >"$home/.cosmon/scheduler.state.json"
printf 'x\n' >"$run_root/checkpoints/service-baseline"

run_phase() {
    date +%s >"$run_root/probes/heartbeat"
    env HOME="$home" PATH="$tools:$PATH" COSMON_WSL2_STATE_ROOT="$state" \
        COSMON_WSL2_RUN_ID="$run_id" MOCK_DISTRIBUTION_BOOT="$1" \
        MOCK_WINDOWS_BOOT_EPOCH="$2" MOCK_RUN_ROOT="$run_root" \
        COSMON_WSL2_TEST_DIRECT_OUTPUT=1 \
        "$verify_script" "$3"
}

wait_for_text() {
    local text="$1" path="$2" attempt
    for attempt in {1..50}; do
        grep -Fq -- "$text" "$path" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}

# A host reboot is witnessed by a strictly newer host boot epoch.
run_phase '2026-10-06 10:00:00' 100 before-reboot >"$work/before-reboot.out"
if run_phase '2026-10-06 11:00:00' 100 after-reboot >"$work/same-reboot.out" 2>&1; then
    echo 'verify-wsl2-host.test: unchanged host boot time was accepted' >&2
    exit 1
fi
grep -Fq 'host did not reboot' "$work/same-reboot.out"
run_phase '2026-10-06 11:00:00' 101 after-reboot >"$work/after-reboot.out"
grep -Fq 'distribution=RESTARTED' "$work/after-reboot.out"

# Every boundary names whether the distribution survived or restarted.
run_phase '2026-10-06 12:00:00' 101 before-logout >/dev/null
run_phase '2026-10-06 12:00:00' 101 after-logout >"$work/logout-survived.out"
grep -Fq 'distribution=SURVIVED' "$work/logout-survived.out"
run_phase '2026-10-06 12:00:00' 101 before-distribution >/dev/null
run_phase '2026-10-06 12:30:00' 101 after-distribution >"$work/distribution-restarted.out"
grep -Fq 'distribution=RESTARTED' "$work/distribution-restarted.out"

# Final refuses an incomplete external matrix and lists every absent checkpoint.
for checkpoint in lifecycle supervisor-crash child-crash timer; do
    printf 'x\n' >"$run_root/checkpoints/$checkpoint"
done
rm -f "$run_root/checkpoints/after-logout" "$run_root/checkpoints/after-distribution" \
    "$run_root/checkpoints/after-reboot" "$run_root/checkpoints/after-sleep"
if run_phase '2026-10-06 12:30:00' 101 final >"$work/final-missing.out" 2>&1; then
    echo 'verify-wsl2-host.test: final accepted missing external checkpoints' >&2
    exit 1
fi
for checkpoint in after-logout after-distribution after-reboot after-sleep; do
    wait_for_text "$checkpoint" "$work/final-missing.out" || {
        cat "$work/final-missing.out" >&2
        echo "verify-wsl2-host.test: final did not list $checkpoint" >&2
        exit 1
    }
done

# Sleep cannot pass if the distribution restarted, and its completion budget
# is measured from probe start rather than from the after-sleep invocation.
run_phase '2026-10-06 12:30:00' 101 before-sleep >"$work/before-sleep.out"
grep -Fq 'keep a WSL client attached' "$work/before-sleep.out"
printf '%s\n' "$(( $(date +%s) - 151 ))" >"$run_root/checkpoints/before-sleep.probe-start-epoch"
rm -f "$run_root/probes/sleep-finished"
start="$(date +%s)"
if run_phase '2026-10-06 12:30:00' 101 after-sleep >"$work/sleep-expired.out" 2>&1; then
    echo 'verify-wsl2-host.test: expired sleep probe budget was accepted' >&2
    exit 1
fi
elapsed=$(( $(date +%s) - start ))
(( elapsed < 5 )) || { echo 'verify-wsl2-host.test: after-sleep restarted its wait budget' >&2; exit 1; }
grep -Fq 'probe completion budget expired' "$work/sleep-expired.out"

printf '%s\n' "$(( $(date +%s) - 1 ))" >"$run_root/checkpoints/before-sleep.probe-start-epoch"
printf 'finished\n' >"$run_root/probes/sleep-finished"
if run_phase '2026-10-06 13:00:00' 101 after-sleep >"$work/sleep-restarted.out" 2>&1; then
    echo 'verify-wsl2-host.test: restarted distribution passed sleep phase' >&2
    exit 1
fi
grep -Fq 'distribution stopped before or during sleep' "$work/sleep-restarted.out"

echo 'verify-wsl2-host.test: cross-boundary evidence contract passed'
