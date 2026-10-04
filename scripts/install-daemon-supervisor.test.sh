#!/usr/bin/env bash
# Exercise binary installation entirely inside a disposable directory.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
linux_work="$(mktemp -d)"
linux_home="$linux_work/home space-%-\$-quote\"-slash\\"
linux_tools="$linux_work/tools"
linux_log="$linux_work/systemctl.log"
mkdir -p "$linux_home/.local/bin" "$linux_home/.config/cosmon" "$linux_tools"

cleanup_linux() {
    rm -rf "$linux_work"
}
trap cleanup_linux EXIT

cat > "$linux_tools/uname" <<'EOF'
#!/bin/sh
echo Linux
EOF
cat > "$linux_tools/systemctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$MOCK_SYSTEMCTL_LOG"
if [ "$1 $2" = "--user show-environment" ] && [ "${MOCK_MANAGER_FAIL:-0}" = 1 ]; then
    exit 1
fi
exit 0
EOF
cat > "$linux_tools/systemd-analyze" <<'EOF'
#!/bin/sh
printf 'analyze %s\n' "$*" >> "$MOCK_SYSTEMCTL_LOG"
[ "${MOCK_VERIFY_FAIL:-0}" != 1 ]
EOF
chmod +x "$linux_tools/uname" "$linux_tools/systemctl" "$linux_tools/systemd-analyze"

cat > "$linux_home/.local/bin/cosmon-daemon-supervisor" <<'EOF'
#!/bin/sh
config=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --config) config="$2"; shift 2 ;;
        --check) shift ;;
        *) shift ;;
    esac
done
[ -s "$config" ] && grep -qx 'valid=true' "$config"
EOF
chmod +x "$linux_home/.local/bin/cosmon-daemon-supervisor"
printf 'valid=true\n' > "$linux_home/.config/cosmon/daemons.toml"

linux_env=(
    "HOME=$linux_home"
    "PATH=$linux_tools:$PATH"
    "MOCK_SYSTEMCTL_LOG=$linux_log"
)

env "${linux_env[@]}" "$script_dir/install-daemon-supervisor.sh" print > "$linux_work/rendered"
grep -Fq 'KillMode=mixed' "$linux_work/rendered"
grep -Fq 'TimeoutStopSec=15s' "$linux_work/rendered"
grep -Fq 'RestartSec=5s' "$linux_work/rendered"
grep -Fq 'WantedBy=default.target' "$linux_work/rendered"
grep -Fq 'home space-%%-$-quote\"-slash' "$linux_work/rendered"
grep -Fq '/usr/bin/env "' "$linux_work/rendered"
grep -Fq '/.local/bin/cosmon-daemon-supervisor" --config ' "$linux_work/rendered"

env "${linux_env[@]}" "$script_dir/install-daemon-supervisor.sh" install
unit="$linux_home/.config/systemd/user/cosmon-daemon-supervisor.service"
[[ -f "$unit" ]]
grep -Fq -- '--user daemon-reload' "$linux_log"
grep -Fq -- '--user enable cosmon-daemon-supervisor.service' "$linux_log"
grep -Fq -- '--user restart cosmon-daemon-supervisor.service' "$linux_log"

# Re-install is idempotent and a changed executable path is rendered on reload.
env "${linux_env[@]}" "$script_dir/install-daemon-supervisor.sh" install
mkdir -p "$linux_home/alternate bin"
cp "$linux_home/.local/bin/cosmon-daemon-supervisor" "$linux_home/alternate bin/cosmon-daemon-supervisor"
env "${linux_env[@]}" COSMON_SUPERVISOR_BIN_DIR="$linux_home/alternate bin" \
    "$script_dir/install-daemon-supervisor.sh" reload
grep -Fq 'alternate bin/cosmon-daemon-supervisor' "$unit"

# Validation failures leave a known-good installed unit byte-for-byte intact.
before="$(shasum -a 256 "$unit")"
printf 'invalid=true\n' > "$linux_home/.config/cosmon/daemons.toml"
if env "${linux_env[@]}" "$script_dir/install-daemon-supervisor.sh" install; then
    echo "install-daemon-supervisor.test: malformed config was accepted" >&2
    exit 1
fi
after="$(shasum -a 256 "$unit")"
[[ "$before" == "$after" ]]
mv "$linux_home/.config/cosmon/daemons.toml" "$linux_home/.config/cosmon/daemons.saved"
if env "${linux_env[@]}" "$script_dir/install-daemon-supervisor.sh" install; then
    echo "install-daemon-supervisor.test: missing config was accepted" >&2
    exit 1
fi
[[ "$before" == "$(shasum -a 256 "$unit")" ]]
mv "$linux_home/.config/cosmon/daemons.saved" "$linux_home/.config/cosmon/daemons.toml"
printf 'valid=true\n' > "$linux_home/.config/cosmon/daemons.toml"
if env "${linux_env[@]}" MOCK_MANAGER_FAIL=1 "$script_dir/install-daemon-supervisor.sh" install \
    >"$linux_work/no-bus.out" 2>"$linux_work/no-bus.err"; then
    echo "install-daemon-supervisor.test: unreachable user manager was accepted" >&2
    exit 1
fi
grep -Fq 'user service manager is unreachable' "$linux_work/no-bus.err"

mkdir -p "$linux_home/.cosmon/logs"
printf 'retain\n' > "$linux_home/.cosmon/logs/operator.log"
printf 'retain\n' > "$linux_home/.cosmon/state.json"
printf 'retain\n' > "$linux_home/.config/systemd/user/unrelated.service"
env "${linux_env[@]}" "$script_dir/install-daemon-supervisor.sh" uninstall
[[ ! -e "$unit" ]]
[[ -f "$linux_home/.config/cosmon/daemons.toml" ]]
[[ -f "$linux_home/.cosmon/logs/operator.log" ]]
[[ -f "$linux_home/.cosmon/state.json" ]]
[[ -f "$linux_home/.config/systemd/user/unrelated.service" ]]
env "${linux_env[@]}" "$script_dir/install-daemon-supervisor.sh" uninstall
echo "install-daemon-supervisor.test: Linux adapter install/reload/uninstall contract passed"

if [[ "$(uname -s)" != Darwin ]]; then
    echo "install-daemon-supervisor.test: macOS signing checks skipped"
    exit 0
fi

work="$(mktemp -d)"
trap 'cleanup_linux; rm -rf "$work"' EXIT

printf 'fn main() {}\n' > "$work/fixture.rs"
rustc --crate-name cosmon_daemon_supervisor "$work/fixture.rs" -o "$work/throwaway"

COSMON_SUPERVISOR_BIN_DIR="$work/prefix/bin" \
COSMON_SUPERVISOR_SIGNING_IDENTITY="Cosmon Test Identity That Does Not Exist" \
    "$script_dir/install-daemon-supervisor.sh" install-binary "$work/throwaway"

installed="$work/prefix/bin/cosmon-daemon-supervisor"
details="$(codesign -dv "$installed" 2>&1)"
requirement="$(codesign -dr - "$installed" 2>&1)"
[[ "$details" == *"Identifier=com.cosmon.daemon-supervisor"* ]] || {
    echo "install-daemon-supervisor.test: identifier was not pinned" >&2
    exit 1
}
[[ "$requirement" == *cdhash* ]] || {
    echo "install-daemon-supervisor.test: ad-hoc fixture did not expose a content-hash requirement" >&2
    exit 1
}
echo "install-daemon-supervisor.test: pinned identifier on throwaway installed binary"

before="$(shasum -a 256 "$installed")"
mkdir -p "$work/failing-tools"
printf '#!/bin/sh\nexit 7\n' > "$work/failing-tools/codesign"
chmod +x "$work/failing-tools/codesign"
if PATH="$work/failing-tools:$PATH" \
    COSMON_SUPERVISOR_BIN_DIR="$work/prefix/bin" \
    COSMON_SUPERVISOR_SIGNING_IDENTITY="Cosmon Test Identity That Does Not Exist" \
    "$script_dir/install-daemon-supervisor.sh" install-binary "$work/throwaway"; then
    echo "install-daemon-supervisor.test: failed signing was accepted" >&2
    exit 1
fi
after="$(shasum -a 256 "$installed")"
[[ "$before" == "$after" ]] || {
    echo "install-daemon-supervisor.test: failed signing replaced the installed binary" >&2
    exit 1
}
echo "install-daemon-supervisor.test: failed signing preserves the installed binary"
