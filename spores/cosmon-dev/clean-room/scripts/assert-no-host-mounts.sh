#!/usr/bin/env bash
# assert-no-host-mounts.sh — fail CLOSED if any run arg would mount a forbidden
# host path into the clean-room. blueprint §4 (jamais monter du Mac).
#
# Usage: assert-no-host-mounts.sh "<all docker run args as one string>"
# Exit 0 = clean. Exit 1 = a forbidden mount was requested (the run must abort).
set -euo pipefail

args="${1:-}"

# The forbidden host paths — mounting any of these leaks the operator's real
# credentials / config / caches into a supposedly clean environment.
forbidden=(
  "${HOME}/.claude"
  "${HOME}/.config"
  "${HOME}/.codex"
  "${HOME}/.ssh"
  "${HOME}/.gitconfig"
  "${HOME}/.cargo"
  "/var/run/docker.sock"
  "/var/run/docker.sock:"
)

# Also refuse a bare $HOME mount and the live checkout / live .cosmon.
forbidden+=( "${HOME}:" "$(pwd)/.cosmon" )

rc=0
for p in "${forbidden[@]}"; do
  if printf '%s' "$args" | grep -Fq -- "$p"; then
    echo "REFUSED: run args request a forbidden host mount: $p" >&2
    rc=1
  fi
done

# A -v/--volume that sources anything under $HOME is refused unless it is an
# explicitly-named disposable volume (docker named volumes have no leading slash).
if printf '%s' "$args" | grep -Eoq -- "-v[= ]${HOME}[^ ]*"; then
  echo "REFUSED: a bind mount sources a path under \$HOME (${HOME})." >&2
  echo "        Use a disposable NAMED volume for ~/.claude (see disposable-login.sh)." >&2
  rc=1
fi

if [ "$rc" -eq 0 ]; then
  echo "assert-no-host-mounts: OK (no forbidden host path in run args)."
fi
exit "$rc"
