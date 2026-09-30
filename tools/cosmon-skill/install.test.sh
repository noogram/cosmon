#!/usr/bin/env bash
# Exercise the cosmon skill installer's destination and linking modes.
#
# Hermetic: every case uses a disposable HOME and, when set, a disposable
# configuration directory. No real user configuration is read or written.

set -uo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALLER="$ROOT_DIR/tools/cosmon-skill/install.sh"
SOURCE="$ROOT_DIR/tools/cosmon-skill/SKILL.md"

pass=0
fail=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; pass=$((pass+1)); }
ko()  { printf '  \033[31m✗\033[0m %s — %s\n' "$1" "$2"; fail=$((fail+1)); }

tmp="$(mktemp -d -t cosmon-skill-install-test-XXXXXX)"
trap 'rm -rf "$tmp"' EXIT

run_install() {
    home="$1"
    config="$2"
    mode="$3"
    if [ -n "$config" ]; then
        OUT=$(HOME="$home" CLAUDE_CONFIG_DIR="$config" bash "$INSTALLER" $mode 2>&1)
    else
        OUT=$(env -u CLAUDE_CONFIG_DIR HOME="$home" bash "$INSTALLER" $mode 2>&1)
    fi
    RC=$?
}

check_case() {
    name="$1"
    home="$2"
    config="$3"
    mode="$4"
    expected_base="${config:-$home/.claude}"
    destination_dir="$expected_base/skills/cosmon"
    destination="$destination_dir/SKILL.md"

    mkdir -p "$home"
    [ -z "$config" ] || mkdir -p "$config"
    run_install "$home" "$config" "$mode"

    [ "$RC" -eq 0 ] && ok "$name installs successfully" \
        || ko "$name installs successfully" "rc=$RC / output=$OUT"
    printf '%s' "$OUT" | grep -Fq "$destination_dir" \
        && ok "$name prints the resolved path" \
        || ko "$name prints the resolved path" "path absent from output"

    if [ "$mode" = "--link" ]; then
        [ -L "$destination" ] \
            && ok "$name creates a symlink" \
            || ko "$name creates a symlink" "target is not a symlink"
        [ "$(readlink "$destination")" = "$SOURCE" ] \
            && ok "$name symlink points at the tracked skill" \
            || ko "$name symlink points at the tracked skill" "target differs"
    else
        [ -f "$destination" ] && [ ! -L "$destination" ] \
            && ok "$name creates a regular file" \
            || ko "$name creates a regular file" "target is not a regular file"
        cmp -s "$SOURCE" "$destination" \
            && ok "$name copies the tracked skill" \
            || ko "$name copies the tracked skill" "content differs"
    fi
}

echo "── cosmon-skill/install.sh ───────────────────────────────────────────"

check_case "HOME copy" "$tmp/home-copy" "" ""
check_case "configured copy" "$tmp/home-configured-copy" "$tmp/configured-copy" ""
check_case "HOME link" "$tmp/home-link" "" "--link"
check_case "configured link" "$tmp/home-configured-link" "$tmp/configured-link" "--link"

echo "──────────────────────────────────────────────────────────────────────"
if [ "$fail" -eq 0 ]; then
    echo "cosmon-skill/install.test: $pass passed, 0 failed."
    exit 0
fi
echo "cosmon-skill/install.test: $pass passed, $fail FAILED." >&2
exit 1
