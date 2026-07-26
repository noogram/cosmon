#!/usr/bin/env bash
# login-in-container.sh — credential path (a): the human logs in, inside the
# container, with their own hands.
#
# This is the ONLY place in this image where a credential comes into
# existence, and it is created by a person at a TTY — never by a script,
# never by an agent, never from a host file, never from an environment
# variable. Nothing here reads, prints, or copies the resulting token: it
# is written by Claude Code itself into $CLAUDE_CONFIG_DIR, owned by uid
# 10001, and it dies when this container is removed.
#
# Invoked as the entrypoint of an interactive, NON-`--rm` container:
#
#   docker run -it --init --name cosmon-mission-live \
#     --entrypoint /usr/local/bin/container-real-mission-login <image>
#
# Then the mission is re-run in that same container with `docker start -ai`.
#
# Reviewers: do not add a non-interactive branch. A login that can run
# unattended is a login whose secret came from somewhere else.
set -euo pipefail

WORKER_UID=10001
WORKER_HOME=/home/cosmon-worker
MISSION_CONFIG="$WORKER_HOME/.claude-mission"

if [ ! -t 0 ]; then
  printf '\033[1;31m✗ no TTY on stdin.\033[0m\n' >&2
  printf 'This entrypoint exists to let a HUMAN complete `/login` by hand.\n' >&2
  printf 'Re-run the container with `-it`.\n' >&2
  exit 1
fi

mkdir -p "$MISSION_CONFIG"
chown -R "$WORKER_UID:$WORKER_UID" "$WORKER_HOME"

printf '\n\033[1;36m▶ starting Claude Code as uid %s with CLAUDE_CONFIG_DIR=%s\033[0m\n' \
  "$WORKER_UID" "$MISSION_CONFIG"
printf '  Complete `/login` in the TUI, then quit it (Ctrl-C twice or /exit).\n'
printf '  The credential is written inside THIS container only.\n\n'

exec setpriv --reuid "$WORKER_UID" --regid "$WORKER_UID" --clear-groups \
  env HOME="$WORKER_HOME" PATH=/usr/local/bin:/usr/bin:/bin \
      CLAUDE_CONFIG_DIR="$MISSION_CONFIG" \
      claude
