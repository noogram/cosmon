#!/usr/bin/env bash
# Run the Linux user-service contracts and falsify both lifetime settings.
set -euo pipefail

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"

fail() {
    echo "linux-user-services-test: $*" >&2
    exit 1
}

offline_check() {
    bash -n "$repo/tests/harness/systemd-supervisor-test.sh"
    bash -n "$repo/tests/harness/systemd-scheduler-test.sh"
    bash -n "$repo/tests/harness/linux-user-services-test.sh"
    grep -Fqx 'KillMode=mixed' "$repo/scripts/systemd/cosmon-daemon-supervisor.service" \
        || fail "supervisor template lost KillMode=mixed"
    grep -Fqx 'KillMode=process' "$repo/scripts/systemd/cosmon-scheduler.service" \
        || fail "scheduler template lost KillMode=process"
    echo "linux-user-services-test: offline contract PASS"
}

if [[ "${1:-}" == --offline-check ]]; then
    offline_check
    exit 0
fi
[[ $# -eq 0 ]] || fail "usage: $0 [--offline-check]"

[[ "$(uname -s)" == Linux ]] || fail "Linux is required"
[[ "${COSMON_SYSTEMD_TEST_ISOLATED:-}" == 1 ]] \
    || fail "refusing to run outside an explicitly isolated test account"
[[ "$HOME" == /tmp/* || "${COSMON_SYSTEMD_TEST_HOME_CONFIRMED:-}" == 1 ]] \
    || fail "test HOME is not recognizably isolated"
systemctl --user show-environment >/dev/null 2>&1 \
    || fail "user service manager is unreachable; this lane must fail, never skip"

unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
for unit in cosmon-daemon-supervisor.service cosmon-scheduler.service cosmon-scheduler.timer; do
    [[ ! -e "$unit_dir/$unit" ]] || fail "isolated account already contains $unit"
done

work="$(mktemp -d "${COSMON_SYSTEMD_TEST_TMPDIR:-${TMPDIR:-/tmp}}/cosmon-linux-services.XXXXXX")"
cleanup() {
    systemctl --user disable --now cosmon-daemon-supervisor.service cosmon-scheduler.timer \
        >/dev/null 2>&1 || true
    systemctl --user stop cosmon-scheduler.service >/dev/null 2>&1 || true
    rm -f "$unit_dir/cosmon-daemon-supervisor.service" \
        "$unit_dir/cosmon-scheduler.service" "$unit_dir/cosmon-scheduler.timer"
    systemctl --user daemon-reload >/dev/null 2>&1 || true
    find "$work" -depth -delete
}
trap cleanup EXIT

run_mutation() {
    local name="$1" template="$2" from="$3" to="$4" expected="$5"
    local mutant="$work/$name" output="$work/$name.log"
    mkdir -p "$mutant/scripts/lib" "$mutant/scripts/systemd" "$mutant/tests/harness"
    cp "$repo/scripts/install-daemon-supervisor.sh" "$repo/scripts/install-scheduler.sh" "$mutant/scripts/"
    cp "$repo/scripts/lib/install-user-service.sh" "$mutant/scripts/lib/"
    cp "$repo/scripts/systemd/"* "$mutant/scripts/systemd/"
    cp "$repo/tests/harness/systemd-supervisor-test.sh" \
        "$repo/tests/harness/systemd-scheduler-test.sh" "$mutant/tests/harness/"
    sed "s/^${from}$/${to}/" "$mutant/scripts/systemd/$template" > "$mutant/scripts/systemd/$template.changed"
    mv "$mutant/scripts/systemd/$template.changed" "$mutant/scripts/systemd/$template"
    if COSMON_SYSTEMD_TEST_TMPDIR="$work" "$mutant/tests/harness/$name" >"$output" 2>&1; then
        fail "$name mutation unexpectedly passed"
    fi
    grep -Fq "$expected" "$output" || {
        sed -n '1,200p' "$output" >&2
        fail "$name mutation failed without the lifetime assertion"
    }
    echo "linux-user-services-test: $name mutation RED ($expected)"
}

COSMON_SYSTEMD_TEST_TMPDIR="$work" "$repo/tests/harness/systemd-supervisor-test.sh"
COSMON_SYSTEMD_TEST_TMPDIR="$work" "$repo/tests/harness/systemd-scheduler-test.sh"

run_mutation systemd-scheduler-test.sh cosmon-scheduler.service \
    'KillMode=process' 'KillMode=control-group' \
    'detached children did not survive tick exit'
run_mutation systemd-supervisor-test.sh cosmon-daemon-supervisor.service \
    'KillMode=mixed' 'KillMode=process' \
    'descendant survived'

for unit in cosmon-daemon-supervisor.service cosmon-scheduler.service cosmon-scheduler.timer; do
    [[ ! -e "$unit_dir/$unit" ]] || fail "$unit remained after the probes"
done
echo "linux-user-services-test: PASS"
