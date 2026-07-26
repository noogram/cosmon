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
# real `cs tackle --adapter claude`, on the production path, with NOTHING
# minted — and therefore it necessarily halts at door 3, the credential
# gate, which is the last step a machine is allowed to take.
#
# A refusal here is the MEASURED OUTCOME, not a failure. The thing under
# measurement is *which* refusal, with *which* reason, leaving *which*
# post-conditions. An exit-0 "success" would be the alarming result.
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
#   0  the gate refused for the expected reason (the expected outcome)
#   1  it did not refuse, or refused for a different reason (a finding)
#   2  the harness itself broke before the gate could be reached
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

# Post-condition: no tmux carcass. `cs` names its own socket and session
# either way, so nothing here is guessed.
ATTACH="$(printf '%s\n' "$TACKLE_OUT" | tr -d '`' \
  | grep -o 'tmux -L [^ ]* \(capture-pane -pS - \|attach \)-t [^ ]*' | head -n1)"
if [ -n "$ATTACH" ]; then
  SOCK="$(printf '%s' "$ATTACH" | awk '{print $3}')"
  SESS="$(printf '%s' "$ATTACH" | awk '{print $NF}')"
  echo "--- MISSION_PANE_AT_RETURN (socket=$SOCK session=$SESS) ---"
  tmux -L "$SOCK" capture-pane -p -t "$SESS" 2>&1
  echo "--- MISSION_PANE_AT_RETURN_END ---"
else
  echo "MISSION_NO_SESSION_NAMED=1"
fi
INNER
chmod a+rx /tmp/mission-inner.sh

mkdir -p "$MISSION_WORK" "$MISSION_CONFIG"
chown -R "$WORKER_UID:$WORKER_UID" "$WORKER_HOME"

say "setpriv --reuid $WORKER_UID  →  cs init / cs nucleate / cs tackle --adapter claude"
sub "CLAUDE_CONFIG_DIR=$MISSION_CONFIG (virgin; no credential minted, none mounted)"
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

# ── 4. Grade the four observable post-conditions of the gate ───────────
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
    --arg cs_version "$(cs --version 2>&1 | head -n1)" \
    --arg claude_version "$(claude --version 2>&1 | head -n1)" \
    --argjson provenance_ok "$PROVENANCE_OK" \
    --argjson refusal_names_credential "$NAMES_CREDENTIAL" \
    --argjson refusal_names_remedy "$NAMES_REMEDY" \
    --argjson no_session_left "$NO_SESSION" \
    --arg refusal_line "$REFUSAL_LINE" \
    '{
       harness: "container-real-mission",
       verdict: $verdict,
       reason: $reason,
       molecule: $molecule,
       topic: $topic,
       tackle_rc: $tackle_rc,
       molecule_status_after: $status,
       cs_version: $cs_version,
       claude_version: $claude_version,
       provenance_ok: ($provenance_ok == 1),
       post_conditions: {
         tackle_exited_non_zero: (($tackle_rc | tonumber? // 0) != 0),
         refusal_names_credential: ($refusal_names_credential == 1),
         refusal_names_remedy: ($refusal_names_remedy == 1),
         no_tmux_session_left: ($no_session_left == 1),
         molecule_not_left_running: ($status != "running")
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

if [ "$INNER_RC" -eq 90 ] || [ -z "$MOL" ]; then
  emit_record "INCONCLUSIVE" "the harness could not nucleate a molecule; the credential gate was never reached"
  exit 2
fi

if [ "${TACKLE_RC:-0}" = "0" ]; then
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
CTX="${COSMON_DOCKER_CONTEXT:-desktop-linux}"
hdr "4. The step that is yours, not the harness's"
cat <<HUMAN
The mission is provisioned and the dispatch path works. It stopped at the
credential gate because this container holds no credential, and this harness
is not allowed to give it one.

To carry the mission the rest of the way, log in YOURSELF, inside a container
of this image, with exactly this one line:

  docker --context $CTX run -it --init --name cosmon-mission-live --entrypoint /usr/local/bin/container-real-mission-login $IMAGE_REF

Then re-run the mission in that same (now authenticated) container:

  docker --context $CTX start -ai cosmon-mission-live

(The harness deletes its image at teardown. Re-run it with COSMON_KEEP_IMAGE=1
if you intend to do the login afterwards.)

The credential written by that login is born inside the container and dies
with it (\`docker rm cosmon-mission-live\`). It never touches your host, and no
agent — including this one — ever sees it.

The alternative, mounting a host credential in, is documented with its costs
in docs/guides/claude-worker-in-a-container.md ("Two ways to put a credential
into the container"). Read it before choosing; the choice is yours to make.
HUMAN
exit 0
