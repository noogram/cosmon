#!/usr/bin/env bash
# install-hooks.sh — put this repo's git hooks in place, and REFUSE rather
# than replace when the destination already holds something else.
#
# Called by `just install`, which `[hooks].post_merge` runs after every
# harvest. That cadence is the whole reason this refuses: a step that runs
# unattended, several times a day, must not mutate a developer's local git
# configuration. An earlier version announced the replacement and did it
# anyway — an announcement nobody reads at 2 a.m. is not consent, and the
# hook it overwrites may be the one a different tool installed.
#
# Three outcomes per hook, and only the first two write anything:
#   absent     → install
#   identical  → nothing to do
#   different  → REFUSE, print both paths, exit non-zero
#
# The list is explicit rather than `hooks/*`. A glob silently enrolls
# whatever lands in the directory next, which is how an install step grows
# reach nobody granted it. `hooks/telegram-notify.sh` is invoked by path and
# is not a git hook, so it is not here — and with an explicit list, that is a
# decision a reader can see rather than an exclusion buried in a filter.
#
# Resolution when it refuses is the caller's, deliberately: inspect the two
# files, keep yours, or `install -m 755 hooks/<name> "$(git rev-parse
# --git-path hooks)/<name>"` to take ours.

set -euo pipefail

HOOKS=(pre-push prepare-commit-msg)

repo_root="${COSMON_HOOKS_SRC_ROOT:-$(git rev-parse --show-toplevel)}"
hooks_dir="${COSMON_HOOKS_DEST:-$(git rev-parse --git-path hooks)}"
mkdir -p "$hooks_dir"

installed=()
kept=()
conflict=0

for name in "${HOOKS[@]}"; do
    src="${repo_root}/hooks/${name}"
    dest="${hooks_dir}/${name}"
    if [ ! -f "$src" ]; then
        echo "install-hooks: MISSING tracked source ${src}" >&2
        conflict=1
        continue
    fi
    if [ ! -e "$dest" ]; then
        install -m 755 "$src" "$dest"
        installed+=("$name")
    elif cmp -s "$src" "$dest"; then
        kept+=("$name")
    else
        echo "install-hooks: REFUSING to replace an existing hook that differs." >&2
        echo "    ours:   ${src}" >&2
        echo "    yours:  ${dest}" >&2
        conflict=1
    fi
done

[ ${#installed[@]} -eq 0 ] || echo "    hooks: installed ${installed[*]}"
[ ${#kept[@]} -eq 0 ]      || echo "    hooks: already current ${kept[*]}"

if [ "$conflict" -ne 0 ]; then
    echo "install-hooks: aborting. Diff the pair above, then keep yours or copy ours" >&2
    echo "  into place explicitly. Nothing was overwritten." >&2
    exit 1
fi
exit 0
