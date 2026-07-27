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
# Invoked with `docker exec -it` into a container that is already running
# under a NEUTRAL entrypoint:
#
#   docker run -dit --init --name cosmon-mission-live \
#     --entrypoint sleep <image> infinity
#   docker exec -it cosmon-mission-live /usr/local/bin/container-real-mission-login
#
# Then the mission runs as a SECOND exec into that same, now-authenticated
# container:
#
#   docker exec -it -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
#     cosmon-mission-live /usr/local/bin/container-real-mission
#
# MEASURED, and the reason the recipe is shaped this way: `docker start`
# does NOT run a different command in an existing container — it replays
# the entrypoint the container was CREATED with. An earlier version of
# this header told the operator to `docker start -ai` after the login and
# promised them the mission; what they got was the login screen a second
# time. `docker exec` is the only way to run a second, different act in
# one long-lived container.
#
# MEASURED too: `CLAUDE_CONFIG_DIR` does not survive between execs. It is
# per-exec, not a property of the container — see the note in
# docs/guides/claude-worker-in-a-container.md. Every exec line that must
# see the credential this login writes has to carry `-e CLAUDE_CONFIG_DIR`
# itself, or the gate looks in `$HOME/.claude` and refuses while the
# credential sits intact thirty centimetres away.
#
# Reviewers: do not add a non-interactive branch. A login that can run
# unattended is a login whose secret came from somewhere else.
set -euo pipefail

WORKER_UID=10001
WORKER_HOME=/home/cosmon-worker
MISSION_CONFIG="$WORKER_HOME/.claude-mission"

if [ ! -t 0 ]; then
  printf '\033[1;31m✗ no TTY on stdin.\033[0m\n' >&2
  printf 'This exists to let a HUMAN complete `/login` by hand.\n' >&2
  printf 'Re-run the exec with `-it`:\n' >&2
  printf '  docker exec -it <container> /usr/local/bin/container-real-mission-login\n' >&2
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
