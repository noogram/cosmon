#!/usr/bin/env bash
# Exercise binary installation entirely inside a disposable directory.
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    echo "install-daemon-supervisor.test: skip (macOS only)"
    exit 0
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

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
