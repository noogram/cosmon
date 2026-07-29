#!/usr/bin/env bash
# in-container-mission.sh — drive ONE real cosmon mission, through the
# production dispatch path, inside the external tester's container, and
# stop at the credential gate.
#
# Runs INSIDE docker/container-real-mission/Dockerfile. It builds its own
# throwaway galaxy under the demote target's home; nothing here can reach
# a host fleet.
#
# ── WHY THIS EXISTS ────────────────────────────────────────────────────
# Every arm of scripts/container-worker-doors-bench.sh stops in front of a
# file literally named PLACEHOLDER-NOT-A-CREDENTIAL. That proves the four
# doors of issue #20 open. It never proved that a real mission walks down
# the corridor. This script is the missing arm: a real `cs nucleate` + a
# real `cs tackle --adapter claude`, on the production path.
#
# ── THE TWO WORLDS ─────────────────────────────────────────────────────
# This script grades against the world it is actually in, and it decides
# which one by stat()ing the credential path — never by opening it.
#
#   no credential present   the mission necessarily halts at door 3, the
#                           credential gate. The refusal is the MEASURED
#                           OUTCOME, not a failure; an exit-0 there would
#                           be the alarming result.
#   credential present      a human has logged in (see
#                           login-in-container.sh). The dispatch is then
#                           entitled to SUCCEED, and the expected outcome
#                           is a spawned, live worker — asserted
#                           positively, never inferred from rc=0 alone.
#
# The first version of this harness only knew the first world, so once the
# human completed the login it reported a failure over a run that worked.
#
# ── SECRET DISCIPLINE (read this before editing) ───────────────────────
# This script never creates, requests, copies, reads, prints or logs any
# credential. It does not even mint the placeholder the doors bench uses:
# the whole point is to arrive at the gate empty-handed. `cs` itself only
# ever stat()s the credentials path; nothing here opens it. Do not add
# ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, a credential mount, or a
# seeded .credentials.json. Their absence IS the measurement.
#
# Output contract:
#   stdout            the raw transcript (every observation verbatim)
#   $OUT_DIR/…        mission-record.json, tackle.out, mission-brief.md
# Exit status:
#   0  the expected outcome for this world was observed —
#      REFUSED-AT-CREDENTIAL-GATE with no credential present, or
#      SPAWNED-LIVE-WORKER with one present
#   1  a finding: the world's expected outcome did not happen
#   2  INCONCLUSIVE — the step that discriminates could not run. Never a
#      silent pass in either world.
set -uo pipefail

OUT_DIR="${OUT_DIR:-/out}"
mkdir -p "$OUT_DIR"

say()  { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
sub()  { printf '\033[0;36m  · %s\033[0m\n' "$*"; }
raw()  { printf '\033[0;90m%s\033[0m\n' "$*"; }
hdr()  { printf '\n\033[1;35m═══ %s ═══\033[0m\n' "$*"; }

WORKER_UID=10001
WORKER_HOME=/home/cosmon-worker
MISSION_WORK="$WORKER_HOME/mission"
# A fresh, isolated config dir. The keychain service name is derived from
# this path, so a new directory is an absent keychain item — which is what
# keeps the arm credential-free by construction rather than by promise.
MISSION_CONFIG="$WORKER_HOME/.claude-mission"

# ── Which world are we in? ─────────────────────────────────────────────
# STAT ONLY. `[ -e ]` is a stat(2); the file is never opened, read,
# printed, logged or copied, and the single bit "it exists" is all that
# leaves this line. The secret discipline above is unchanged — this adds
# no way to obtain a credential, only a way to notice that the human
# already created one with their own hands.
#
# The path is the one `cs` itself probes: `$CLAUDE_CONFIG_DIR` +
# `/.credentials.json` (crates/cosmon-transport/src/claude_login.rs). In
# this image the plaintext backend is the only one available — there is
# no dbus session and so no secret-service keychain — which is why a file
# stat is a sufficient discriminator HERE and would not be on a desktop.
MISSION_CREDENTIAL="$MISSION_CONFIG/.credentials.json"
if [ -e "$MISSION_CREDENTIAL" ]; then
  CREDENTIAL_PRESENT=1
else
  CREDENTIAL_PRESENT=0
fi

# The payload. Deliberately target-agnostic and small: this molecule is
# about the corridor, not about what is carried down it. Any real task
# would exercise the same dispatch path.
MISSION_TOPIC="${MISSION_TOPIC:-add a one-line usage example to the README}"

# ── 1. Environment fidelity + provenance ───────────────────────────────
hdr "1. Environment and provenance"
sub "uname -a"
raw "$(uname -a)"
sub "cs --version"
raw "$(cs --version 2>&1)"
sub "claude --version"
raw "$(claude --version 2>&1 || echo '(claude --version failed)')"

# `cs --version` cannot distinguish our branch from the v0.3.0 tag the
# tester ran, so the binary is identified by strings the fixes introduced.
# Without this, a refusal is ambiguous between "the gate is present and
# fired" and "some older code path happened to fail".
sub "provenance: fix-only strings that do not exist in v0.3.0"
PROVENANCE_OK=1
for marker in "no usable Claude Code credential" "hasTrustDialogAccepted" "awaiting-human"; do
  if grep -aqF "$marker" /usr/local/bin/cs; then
    raw "PRESENT  \"$marker\""
  else
    raw "ABSENT   \"$marker\"  ← the binary under test is NOT the fixed branch"
    PROVENANCE_OK=0
  fi
done

# ── 2. The mission brief — a real molecule, written by hand ────────────
hdr "2. The mission"
sub "topic: $MISSION_TOPIC"
{
  printf '# Mission (container real-dispatch harness)\n\n'
  printf 'Topic: %s\n\n' "$MISSION_TOPIC"
  printf 'Dispatched by `cs tackle --adapter claude` as uid %s inside the\n' "$WORKER_UID"
  printf 'issue-20 replay image, with no credential of any kind provisioned.\n'
} >"$OUT_DIR/mission-brief.md"

# ── 3. Drive the production path as the worker uid ─────────────────────
# One demoted shell does galaxy + nucleate + tackle, so every artefact is
# created by uid 10001 itself — the tester's shape, not a root-created
# tree handed over afterwards.
cat >/tmp/mission-inner.sh <<'INNER'
set -uo pipefail
cd "$MISSION_WORK"
git config --global --add safe.directory "$MISSION_WORK" 2>/dev/null || true
git init -q
git config user.name  "cosmon container mission harness"
git config user.email "container-mission@cosmon.invalid"
git commit -q --allow-empty -m "empty base commit"
cs init >/dev/null 2>&1
git add -A && git commit -qm "cs init" >/dev/null 2>&1

MOL="$(cs nucleate task-work --json --var topic="$MISSION_TOPIC" \
  2>/dev/null | jq -r 'select(.id != null) | .id' | grep '^task-' | head -n1)"
echo "MISSION_MOL=$MOL"
[ -n "$MOL" ] || exit 90

echo "--- cs tackle $MOL --adapter claude (the production dispatch path) ---"
TACKLE_OUT="$(timeout 240 cs tackle "$MOL" --adapter claude 2>&1)"
TACKLE_RC=$?
printf '%s\n' "$TACKLE_OUT"
echo "MISSION_TACKLE_RC=$TACKLE_RC"

# Post-condition: the molecule must NOT be left `running`. A gate that
# refuses but leaves a molecule marked running has traded a mute hang for
# a lying ledger.
echo "MISSION_STATUS=$(cs observe "$MOL" --json 2>/dev/null \
  | jq -r '.status // .molecule.status // "?"' 2>/dev/null | head -n1)"

# Post-condition: the tmux session. In the refusing world its ABSENCE is
# what is being checked (no carcass); in the authenticated world its
# PRESENCE is what proves a worker is actually live rather than merely
# exited-zero. Either way `cs` names its own socket and session, so
# nothing here is guessed — and `has-session` is a positive probe, not an
# inference from the tackle's exit code.
ATTACH="$(printf '%s\n' "$TACKLE_OUT" | tr -d '`' \
  | grep -o 'tmux -L [^ ]* \(capture-pane -pS - \|attach \)-t [^ ]*' | head -n1)"
if [ -n "$ATTACH" ]; then
  SOCK="$(printf '%s' "$ATTACH" | awk '{print $3}')"
  SESS="$(printf '%s' "$ATTACH" | awk '{print $NF}')"
  echo "MISSION_SOCKET=$SOCK"
  echo "MISSION_SESSION=$SESS"
  if tmux -L "$SOCK" has-session -t "$SESS" 2>/dev/null; then
    echo "MISSION_SESSION_ALIVE=1"
  else
    echo "MISSION_SESSION_ALIVE=0"
  fi
  echo "--- MISSION_PANE_AT_RETURN (socket=$SOCK session=$SESS) ---"
  tmux -L "$SOCK" capture-pane -p -t "$SESS" 2>&1
  echo "--- MISSION_PANE_AT_RETURN_END ---"
else
  echo "MISSION_NO_SESSION_NAMED=1"
  echo "MISSION_SESSION_ALIVE=0"
fi
INNER
chmod a+rx /tmp/mission-inner.sh

mkdir -p "$MISSION_WORK" "$MISSION_CONFIG"
chown -R "$WORKER_UID:$WORKER_UID" "$WORKER_HOME"

say "setpriv --reuid $WORKER_UID  →  cs init / cs nucleate / cs tackle --adapter claude"
if [ "$CREDENTIAL_PRESENT" -eq 1 ]; then
  sub "CLAUDE_CONFIG_DIR=$MISSION_CONFIG (a credential is PRESENT — stat only, never opened)"
  sub "world: authenticated. Expected outcome: a spawned, LIVE worker."
else
  sub "CLAUDE_CONFIG_DIR=$MISSION_CONFIG (virgin; no credential minted, none mounted)"
  sub "world: empty-handed. Expected outcome: REFUSED-AT-CREDENTIAL-GATE."
fi
MISSION_OUT="$(setpriv --reuid "$WORKER_UID" --regid "$WORKER_UID" --clear-groups \
  env HOME="$WORKER_HOME" PATH=/usr/local/bin:/usr/bin:/bin \
      CLAUDE_CONFIG_DIR="$MISSION_CONFIG" \
      MISSION_WORK="$MISSION_WORK" MISSION_TOPIC="$MISSION_TOPIC" \
      bash /tmp/mission-inner.sh 2>&1)"
INNER_RC=$?
printf '%s\n' "$MISSION_OUT" >"$OUT_DIR/tackle.out"
raw "$MISSION_OUT"

MOL="$(printf '%s\n' "$MISSION_OUT" | sed -n 's/^MISSION_MOL=//p' | head -n1)"
TACKLE_RC="$(printf '%s\n' "$MISSION_OUT" | sed -n 's/^MISSION_TACKLE_RC=//p' | head -n1)"
STATUS="$(printf '%s\n' "$MISSION_OUT" | sed -n 's/^MISSION_STATUS=//p' | head -n1)"
SESSION_ALIVE="$(printf '%s\n' "$MISSION_OUT" | sed -n 's/^MISSION_SESSION_ALIVE=//p' | head -n1)"
SESSION_NAME="$(printf '%s\n' "$MISSION_OUT" | sed -n 's/^MISSION_SESSION=//p' | head -n1)"

# ── 4. Grade the observable post-conditions, in the world we are in ────
hdr "3. Verdict"

emit_record() {
  local verdict="$1" reason="$2"
  jq -n \
    --arg verdict "$verdict" \
    --arg reason "$reason" \
    --arg molecule "${MOL:-}" \
    --arg tackle_rc "${TACKLE_RC:-}" \
    --arg status "${STATUS:-}" \
    --arg topic "$MISSION_TOPIC" \
    --arg session "${SESSION_NAME:-}" \
    --arg cs_version "$(cs --version 2>&1 | head -n1)" \
    --arg claude_version "$(claude --version 2>&1 | head -n1)" \
    --argjson provenance_ok "$PROVENANCE_OK" \
    --argjson credential_present "$CREDENTIAL_PRESENT" \
    --argjson refusal_names_credential "$NAMES_CREDENTIAL" \
    --argjson refusal_names_remedy "$NAMES_REMEDY" \
    --argjson no_session_left "$NO_SESSION" \
    --argjson session_alive "${SESSION_ALIVE:-0}" \
    --arg refusal_line "$REFUSAL_LINE" \
    '{
       harness: "container-real-mission",
       verdict: $verdict,
       reason: $reason,
       # Which world this run was graded against. Determined by stat()ing
       # the credentials path — never by opening it.
       world: (if $credential_present == 1 then "credential-present"
               else "no-credential" end),
       credential_present: ($credential_present == 1),
       molecule: $molecule,
       topic: $topic,
       tackle_rc: $tackle_rc,
       molecule_status_after: $status,
       tmux_session: $session,
       cs_version: $cs_version,
       claude_version: $claude_version,
       provenance_ok: ($provenance_ok == 1),
       post_conditions: {
         tackle_exited_non_zero: (($tackle_rc | tonumber? // 0) != 0),
         refusal_names_credential: ($refusal_names_credential == 1),
         refusal_names_remedy: ($refusal_names_remedy == 1),
         no_tmux_session_left: ($no_session_left == 1),
         tmux_session_alive: ($session_alive == 1),
         molecule_not_left_running: ($status != "running"),
         molecule_no_longer_pending: ($status != "pending")
       },
       refusal_line: $refusal_line,
       secrets_touched: "none — no credential created, read, mounted or logged"
     }' >"$OUT_DIR/mission-record.json"
  printf '\n\033[1;33mVERDICT %s — %s\033[0m\n' "$verdict" "$reason"
  cat "$OUT_DIR/mission-record.json"
}

NAMES_CREDENTIAL=0
NAMES_REMEDY=0
NO_SESSION=0
REFUSAL_LINE=""

if printf '%s' "$MISSION_OUT" | grep -qF "no usable Claude Code credential"; then
  NAMES_CREDENTIAL=1
  REFUSAL_LINE="$(printf '%s\n' "$MISSION_OUT" \
    | grep -F "no usable Claude Code credential" | head -n1)"
fi
printf '%s' "$MISSION_OUT" | grep -qF "CLAUDE_CODE_OAUTH_TOKEN" && NAMES_REMEDY=1
printf '%s' "$MISSION_OUT" | grep -qF "MISSION_NO_SESSION_NAMED=1" && NO_SESSION=1
printf '%s' "$MISSION_OUT" | grep -qF "no server running on" && NO_SESSION=1

# Shared by both worlds: without a molecule there is nothing to grade at
# all, in either of them.
if [ "$INNER_RC" -eq 90 ] || [ -z "$MOL" ]; then
  emit_record "INCONCLUSIVE" "the harness could not nucleate a molecule; the dispatch path was never entered"
  exit 2
fi
if [ -z "${TACKLE_RC:-}" ]; then
  emit_record "INCONCLUSIVE" \
    "the inner shell reported no tackle exit code (inner_rc=$INNER_RC); nothing can be graded — read tackle.out"
  exit 2
fi

if [ "$CREDENTIAL_PRESENT" -eq 1 ]; then
  # ── World B: a human logged in. A successful dispatch is the expected
  # outcome, and it is asserted POSITIVELY. rc=0 alone proves only that a
  # process exited; it does not prove a worker exists.
  if [ "$TACKLE_RC" != "0" ]; then
    emit_record "REFUSED-WITH-CREDENTIAL" \
      "a credential is present yet cs tackle refused (rc=$TACKLE_RC) — a finding. If the refusal names the credential, the likely cause is CLAUDE_CONFIG_DIR not being carried into this exec (it is per-exec, never inherited): re-run with -e CLAUDE_CONFIG_DIR=$MISSION_CONFIG"
    exit 1
  fi
  if [ -z "$SESSION_NAME" ]; then
    emit_record "INCONCLUSIVE" \
      "cs tackle exited 0 but named no tmux session, so worker liveness could not be probed — and a zero exit code is not evidence of a live worker. Read tackle.out"
    exit 2
  fi
  if [ "${SESSION_ALIVE:-0}" != "1" ]; then
    emit_record "SPAWNED-BUT-DEAD" \
      "cs tackle exited 0 and named session '$SESSION_NAME', but tmux has-session says it is not there — the worker did not survive the dispatch"
    exit 1
  fi
  if [ "$STATUS" = "pending" ] || [ -z "$STATUS" ]; then
    emit_record "SPAWNED-BUT-LEDGER-UNMOVED" \
      "a live worker is in session '$SESSION_NAME', but the molecule is still '${STATUS:-?}' — the ledger did not follow the dispatch"
    exit 1
  fi
  emit_record "SPAWNED-LIVE-WORKER" \
    "with a credential present, cs tackle exited 0, tmux session '$SESSION_NAME' answers has-session, and the molecule is no longer pending (status=$STATUS)"
  exit 0
fi

# ── World A: empty-handed. The refusal IS the measurement.
if [ "$TACKLE_RC" = "0" ]; then
  emit_record "NOT-REFUSED" \
    "cs tackle exited 0 with no credential provisioned — the gate did NOT hold (this is a finding, not a pass)"
  exit 1
fi

if [ "$NAMES_CREDENTIAL" -ne 1 ]; then
  emit_record "REFUSED-OTHER" \
    "cs tackle refused (rc=$TACKLE_RC) but did not name the credential; the corridor stopped at a different door — read tackle.out"
  exit 1
fi

emit_record "REFUSED-AT-CREDENTIAL-GATE" \
  "cs tackle refused with rc=$TACKLE_RC for the expected reason: no usable Claude Code credential"

# ── 5. The one step a machine must not take ────────────────────────────
# IMAGE_REF is injected by the host driver so the printed line is
# copy-pasteable rather than approximately right.
IMAGE_REF="${IMAGE_REF:-cosmon-container-real-mission:bench}"
# The host driver injects the context it actually used, so the printed line
# names the engine this run happened on. The fallback is the bench profile's
# context (corrected 2026-07-27 from `desktop-linux`, which was never the
# tester's engine — see docs/benches/engine-fidelity-2026-07-27.md), not
# whatever context is merely current.
CTX="${COSMON_DOCKER_CONTEXT:-colima-cosmon-bench}"
hdr "4. The step that is yours, not the harness's"
cat <<HUMAN
The mission is provisioned and the dispatch path works. It stopped at the
credential gate because this container holds no credential, and this harness
is not allowed to give it one.

To carry the mission the rest of the way, keep ONE long-lived container alive
under a neutral entrypoint, and enter it once per act with \`docker exec\`:

  # 1. a container that just sits there. The entrypoint is deliberately inert.
  docker --context $CTX run -dit --init --name cosmon-mission-live --entrypoint sleep $IMAGE_REF infinity

  # 2. the gesture that is YOURS: complete /login at a real TTY, then quit it.
  docker --context $CTX exec -it cosmon-mission-live /usr/local/bin/container-real-mission-login

  # 3. now re-run the mission in that same, authenticated container.
  #    CLAUDE_CONFIG_DIR is PER-EXEC — it is not a property of the container,
  #    and an exec that omits it makes the gate look in the wrong directory.
  docker --context $CTX exec -it -e CLAUDE_CONFIG_DIR=$MISSION_CONFIG -e MISSION_TOPIC="$MISSION_TOPIC" cosmon-mission-live /usr/local/bin/container-real-mission

  # 4. a second door, to watch the worker while it runs. \`cs peek\` with NO
  #    argument gives the fleet watchdog view, which is the one worth having.
  docker --context $CTX exec -it -u $WORKER_UID -w $MISSION_WORK -e CLAUDE_CONFIG_DIR=$MISSION_CONFIG cosmon-mission-live bash

Do NOT use \`docker start -ai\` for step 3. MEASURED: \`docker start\` replays the
entrypoint the container was CREATED with, so it would hand you the login a
second time instead of the mission. \`docker exec\` is what runs a second,
different act inside one container.

(The harness deletes its image at teardown. Re-run it with COSMON_KEEP_IMAGE=1
if you intend to do the login afterwards.)

The credential written by that login is born inside the container and dies
with it (\`docker rm -f cosmon-mission-live\`). It never touches your host, and no
agent — including this one — ever sees it.

The alternative, mounting a host credential in, is documented with its costs
in docs/guides/claude-worker-in-a-container.md ("Two ways to put a credential
into the container"). Read it before choosing; the choice is yours to make.
HUMAN
exit 0
