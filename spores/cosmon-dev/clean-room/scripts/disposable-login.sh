#!/usr/bin/env bash
# disposable-login.sh — subscription login in a THROWAWAY volume, scrubbed on exit.
# blueprint §4/K5: login par `claude /login` dans un volume jetable monte en
# ~/.claude, ANTHROPIC_API_KEY vide, leak-check au demarrage, JAMAIS le ~/.claude
# du Mac.
#
# Usage: disposable-login.sh <image-digest>
#   Creates a fresh docker named volume, runs `claude /login` with it mounted at
#   /home/worker/.claude, then leaves the volume name on stdout for the caller to
#   pass to run-clean-room.sh. Call `disposable-login.sh --scrub <volume>` to
#   destroy it when the mission ends.
set -euo pipefail

IMAGE="${1:?usage: disposable-login.sh <image-digest> | --scrub <volume>}"

if [ "$IMAGE" = "--scrub" ]; then
  vol="${2:?usage: disposable-login.sh --scrub <volume>}"
  docker volume rm -f "$vol" >/dev/null
  echo "scrubbed disposable login volume: $vol" >&2
  exit 0
fi

# A per-run throwaway volume name. No timestamp from the shell clock is required;
# the docker volume id is unique enough. Caller keeps the printed name.
vol="cosmon-dev-login-$(head -c8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
docker volume create "$vol" >/dev/null

# Leak-check: refuse to proceed if ANTHROPIC_API_KEY is set in this shell (a key
# leaking into the container would be a second, non-subscription credential).
if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  echo "REFUSED: ANTHROPIC_API_KEY is set in this shell; unset it before a" >&2
  echo "        subscription login (the disposable login is subscription-only)." >&2
  docker volume rm -f "$vol" >/dev/null
  exit 1
fi

echo "disposable login volume: $vol (mounting at /home/worker/.claude)" >&2
# Interactive login into the disposable volume. ANTHROPIC_API_KEY forced empty in
# the container; the Mac's ~/.claude is NEVER mounted.
docker run --rm -it \
  -u 10001 \
  -e ANTHROPIC_API_KEY="" \
  -e DISABLE_AUTOUPDATER=1 \
  -v "$vol:/home/worker/.claude" \
  "$IMAGE" \
  /bin/bash -lc 'claude /login'

# Emit the volume name for the caller (run-clean-room.sh --login-volume <vol>).
echo "$vol"
