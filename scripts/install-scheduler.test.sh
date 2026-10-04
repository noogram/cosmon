#!/usr/bin/env bash
# Exercise scheduler service installation inside a disposable directory.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
home="$work/home space-%-\$-quote\"-slash\\"
tools="$work/tools"
manager_log="$work/systemctl.log"
mkdir -p "$home/.local/bin" "$home/.config/cosmon" "$tools"
trap 'rm -rf "$work"' EXIT

cat > "$tools/uname" <<'EOF'
#!/bin/sh
echo Linux
EOF
cat > "$tools/systemctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$MOCK_SYSTEMCTL_LOG"
if [ "$1 $2" = "--user show-environment" ] && [ "${MOCK_MANAGER_FAIL:-0}" = 1 ]; then
    exit 1
fi
exit 0
EOF
cat > "$tools/systemd-analyze" <<'EOF'
#!/bin/sh
printf 'analyze %s\n' "$*" >> "$MOCK_SYSTEMCTL_LOG"
[ "${MOCK_VERIFY_FAIL:-0}" != 1 ]
EOF
chmod +x "$tools/uname" "$tools/systemctl" "$tools/systemd-analyze"

cat > "$home/.local/bin/cosmon-scheduler" <<'EOF'
#!/bin/sh
[ "${1:-}" = validate ] || exit 0
[ "${2:-}" = --config ] || exit 1
[ -s "${3:-}" ] && grep -qx 'valid=true' "$3"
EOF
chmod +x "$home/.local/bin/cosmon-scheduler"
printf 'valid=true\n' > "$home/.config/cosmon/patrols.toml"

test_env=(
    "HOME=$home"
    "PATH=$tools:$PATH"
    "MOCK_SYSTEMCTL_LOG=$manager_log"
)

env "${test_env[@]}" "$script_dir/install-scheduler.sh" print > "$work/rendered"
grep -Fq 'Type=oneshot' "$work/rendered"
grep -Fq 'KillMode=process' "$work/rendered"
grep -Fq 'OnActiveSec=60s' "$work/rendered"
grep -Fq 'OnUnitActiveSec=60s' "$work/rendered"
grep -Fq 'AccuracySec=1s' "$work/rendered"
grep -Fq 'RandomizedDelaySec=0' "$work/rendered"
grep -Fq 'Persistent=false' "$work/rendered"
grep -Fq 'WantedBy=timers.target' "$work/rendered"
grep -Fq 'home space-%%-$-quote\"-slash' "$work/rendered"
grep -Fq '/usr/bin/env "' "$work/rendered"
grep -Fq '/.local/bin/cosmon-scheduler" tick --config ' "$work/rendered"

env "${test_env[@]}" "$script_dir/install-scheduler.sh" install
service="$home/.config/systemd/user/cosmon-scheduler.service"
timer="$home/.config/systemd/user/cosmon-scheduler.timer"
[[ -f "$service" && -f "$timer" ]]
grep -Fq -- '--user daemon-reload' "$manager_log"
grep -Fq -- '--user enable cosmon-scheduler.timer' "$manager_log"
grep -Fq -- '--user restart cosmon-scheduler.timer' "$manager_log"

# Re-install is idempotent and a changed executable path is rendered on reload.
env "${test_env[@]}" "$script_dir/install-scheduler.sh" install
mkdir -p "$home/alternate bin"
cp "$home/.local/bin/cosmon-scheduler" "$home/alternate bin/cosmon-scheduler"
env "${test_env[@]}" COSMON_SCHEDULER_BIN_DIR="$home/alternate bin" \
    "$script_dir/install-scheduler.sh" reload
grep -Fq 'alternate bin/cosmon-scheduler' "$service"

# Every preflight failure preserves both known-good unit files byte-for-byte.
before_service="$(shasum -a 256 "$service")"
before_timer="$(shasum -a 256 "$timer")"
printf 'invalid=true\n' > "$home/.config/cosmon/patrols.toml"
if env "${test_env[@]}" "$script_dir/install-scheduler.sh" install; then
    echo "install-scheduler.test: malformed config was accepted" >&2
    exit 1
fi
[[ "$before_service" == "$(shasum -a 256 "$service")" ]]
[[ "$before_timer" == "$(shasum -a 256 "$timer")" ]]
mv "$home/.config/cosmon/patrols.toml" "$home/.config/cosmon/patrols.saved"
if env "${test_env[@]}" "$script_dir/install-scheduler.sh" install; then
    echo "install-scheduler.test: missing config was accepted" >&2
    exit 1
fi
[[ "$before_service" == "$(shasum -a 256 "$service")" ]]
[[ "$before_timer" == "$(shasum -a 256 "$timer")" ]]
mv "$home/.config/cosmon/patrols.saved" "$home/.config/cosmon/patrols.toml"
printf 'valid=true\n' > "$home/.config/cosmon/patrols.toml"
if env "${test_env[@]}" MOCK_VERIFY_FAIL=1 "$script_dir/install-scheduler.sh" install; then
    echo "install-scheduler.test: invalid staged units were accepted" >&2
    exit 1
fi
[[ "$before_service" == "$(shasum -a 256 "$service")" ]]
[[ "$before_timer" == "$(shasum -a 256 "$timer")" ]]
if env "${test_env[@]}" MOCK_MANAGER_FAIL=1 "$script_dir/install-scheduler.sh" install \
    >"$work/no-bus.out" 2>"$work/no-bus.err"; then
    echo "install-scheduler.test: unreachable user manager was accepted" >&2
    exit 1
fi
grep -Fq 'user service manager is unreachable' "$work/no-bus.err"

mkdir -p "$home/.cosmon"
printf 'retain\n' > "$home/.cosmon/scheduler.state.json"
printf 'retain\n' > "$home/.config/systemd/user/unrelated.service"
env "${test_env[@]}" "$script_dir/install-scheduler.sh" uninstall
[[ ! -e "$service" && ! -e "$timer" ]]
[[ -f "$home/.config/cosmon/patrols.toml" ]]
[[ -f "$home/.cosmon/scheduler.state.json" ]]
[[ -f "$home/.config/systemd/user/unrelated.service" ]]
grep -Fq -- '--user disable --now cosmon-scheduler.timer' "$manager_log"
grep -Fq -- '--user stop cosmon-scheduler.service' "$manager_log"
env "${test_env[@]}" "$script_dir/install-scheduler.sh" uninstall

grep -Fq '<key>AbandonProcessGroup</key>' \
    "$script_dir/launchd/com.cosmon.scheduler.plist"
echo "install-scheduler.test: install/reload/uninstall contract passed"
