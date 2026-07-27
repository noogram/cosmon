#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# claude-pregrant-discriminator.sh — decide, WITHOUT ANY CREDENTIAL, whether a
# virgin CLAUDE_CONFIG_DIR needs to be *matured* by a prior `claude` run before
# cosmon's startup-consent pre-grant can carry a TUI worker to a composer.
#
# Why this script exists
# ---------------------
# On COSMON-DEV issue #20 the external tester measured ~20 spawns from a fresh
# config directory, none of which reached a composer, and one hands-off success
# on a directory that had been matured by roughly four prior `claude -p` runs.
# That correlation supports two incompatible readings:
#
#   (M) maturation is REQUIRED — Claude Code rewrites the config from its own
#       in-memory state on what it considers a first run, so a pre-grant seeded
#       into a virgin directory is destroyed before the render decision;
#   (P) the pre-grant is SUFFICIENT — maturation is incidental, and whatever
#       blocks that bench is a different door.
#
# Distinguishing them normally looks like it needs a working login, because
# "did it reach a composer" sounds like a question about a session that runs.
# It is not. The onboarding wizard is painted *before* Claude Code cares whether
# it is logged in, so an unauthenticated launch still answers the question: a
# virgin directory paints `Let's get started.`, while a directory whose consent
# is honoured paints the composer with `Not logged in · Run /login` in the
# status line. Those two screens are what this script separates, and neither
# requires a secret. Nothing here reads, writes, copies or prints a credential;
# the only credential-shaped operation is the `stat` in `report_credential`,
# which never opens the file.
#
# What it reports
# ---------------
# Three arms against the SAME workspace and the same installed `claude`:
#
#   V0  virgin config dir, no pre-grant           — the control; must show the wizard
#   V1  virgin config dir + pre-grant             — the reading (P) predicts a composer
#   M1  matured by one `claude -p`, + pre-grant   — the reading (M) predicts this is
#                                                   the ONLY arm that composes
#
# V1 composing refutes (M) on the bed that ran it. V1 showing the wizard while
# M1 composes confirms (M) there, and the fix is then a maturation step in the
# spawn path. V0 composing means the bed is not virgin and the run is void —
# the script says so rather than reporting three green arms over a dirty bed.
#
# The arms are deliberately not graded by grepping for success. A correct
# refusal and a broken harness look identical to a grep, which is the failure
# that already cost this issue one wasted round trip; each arm is classified by
# the marker it *shows*, and an arm matching no known marker is reported as
# UNKNOWN with the pane quoted, never silently bucketed into one of the two.
#
# Usage:  scripts/claude-pregrant-discriminator.sh [workspace-dir]
# Exit:   0 the run produced a verdict, 1 the run is void (see the message).

set -uo pipefail

WORKSPACE=${1:-}
CLAUDE_BIN=${CLAUDE_BIN:-claude}
# Long enough for a cold arm64 container to paint its first frame; the tester's
# bed is the slow one, and a short settle would report UNKNOWN there for timing
# reasons that have nothing to do with the question being asked.
SETTLE=${SETTLE:-15}

die() { printf 'discriminator: %s\n' "$*" >&2; exit 1; }

command -v tmux >/dev/null 2>&1 || die "tmux is required"
command -v "$CLAUDE_BIN" >/dev/null 2>&1 || die "no '$CLAUDE_BIN' on PATH (set CLAUDE_BIN)"

BENCH=$(mktemp -d "${TMPDIR:-/tmp}/claude-pregrant-discriminator.XXXXXX") || die "mktemp failed"
# A tmux socket named for this run only. A shared socket name is a shared
# resource: killing "the" server has already destroyed a live cosmon mission
# once on this issue, so this server is private and is torn down by name.
SOCKET="pregrant-disc-$$"

cleanup() {
    tmux -L "$SOCKET" kill-server >/dev/null 2>&1
    [ -n "${KEEP_BENCH:-}" ] || rm -rf "$BENCH"
}
trap cleanup EXIT

# A throwaway workspace unless the caller named one. Claude Code keys folder
# trust on the absolute resolved path, so every arm must use the identical
# string — `pwd -P` here and canonicalisation in cosmon agree on that.
if [ -z "$WORKSPACE" ]; then
    WORKSPACE="$BENCH/ws"
    mkdir -p "$WORKSPACE" || die "cannot create workspace"
fi
WORKSPACE=$(cd "$WORKSPACE" && pwd -P) || die "cannot resolve workspace"

# --- the pre-grant, byte-for-byte the key set crates/cosmon-transport/src/claude_trust.rs writes ---
# Reimplemented in shell on purpose: the point of the bench is to test the KEYS
# against an installed binary on a foreign bed, not to test cosmon's Rust. A bed
# without a cosmon build can still run it.
pregrant() {
    local cfg=$1 tmp
    mkdir -p "$cfg" || return 1
    [ -f "$cfg/.claude.json" ] || printf '{}' > "$cfg/.claude.json" || return 1
    tmp=$(mktemp "$cfg/.claude.json.XXXXXX") || return 1
    jq --arg ws "$WORKSPACE" \
       '.hasCompletedOnboarding = true | .projects[$ws].hasTrustDialogAccepted = true' \
       "$cfg/.claude.json" > "$tmp" || { rm -f "$tmp"; return 1; }
    mv "$tmp" "$cfg/.claude.json" || return 1

    [ -f "$cfg/settings.json" ] || printf '{}' > "$cfg/settings.json" || return 1
    tmp=$(mktemp "$cfg/settings.json.XXXXXX") || return 1
    jq '.skipDangerousModePermissionPrompt = true' "$cfg/settings.json" > "$tmp" \
        || { rm -f "$tmp"; return 1; }
    mv "$tmp" "$cfg/settings.json" || return 1
}

# One `claude -p` against the config dir. It is expected to FAIL on a bed with
# no credential — that is the point. What matters is the state it leaves behind
# (machineID / userID / migration keys), which is what "matured" means here.
mature() {
    local cfg=$1
    ( cd "$WORKSPACE" && CLAUDE_CONFIG_DIR="$cfg" \
        timeout 120 "$CLAUDE_BIN" -p 'discriminator probe' >/dev/null 2>&1 )
    return 0
}

# Did the directory actually acquire first-run state? Reported per arm so a
# reader can see whether "matured" meant anything on this bed, instead of
# trusting that the invocation did what its name says.
matured_p() {
    local cfg=$1
    [ -f "$cfg/.claude.json" ] || { echo no; return; }
    if [ "$(jq -r 'has("machineID") and has("userID")' "$cfg/.claude.json" 2>/dev/null)" = true ]
    then echo yes; else echo no; fi
}

# Stat-only. Never opens the file: this bench must be safe to run on a bed whose
# operator has a real credential sitting in the directory it points at.
report_credential() {
    local cfg=$1
    if [ -e "$cfg/.credentials.json" ]; then echo "present (not read)"; else echo absent; fi
}

launch_and_capture() {
    local arm=$1 cfg=$2
    tmux -L "$SOCKET" new-session -d -s "$arm" -x 120 -y 40 -c "$WORKSPACE" \
        "CLAUDE_CONFIG_DIR=$cfg $CLAUDE_BIN --permission-mode bypassPermissions" \
        >/dev/null 2>&1 || return 1
    sleep "$SETTLE"
    tmux -L "$SOCKET" capture-pane -t "$arm" -p > "$BENCH/$arm.pane" 2>/dev/null
    tmux -L "$SOCKET" kill-session -t "$arm" >/dev/null 2>&1
    return 0
}

# Classify by the marker shown. An unmatched pane is UNKNOWN, never folded into
# a neighbour: the whole point of the run is that an unrecognised screen is a
# finding, and a classifier that always picks a side manufactures agreement.
classify() {
    local pane=$1
    if grep -qF "Let's get started" "$pane" || grep -qF "Choose the text style" "$pane"; then
        echo WIZARD
    elif grep -qF "Quick safety check" "$pane"; then
        echo TRUST_DIALOG
    elif grep -qF "Bypass Permissions mode" "$pane"; then
        echo BYPASS_DISCLAIMER
    elif grep -qF "for shortcuts" "$pane" || grep -qF "bypass permissions on" "$pane"; then
        echo COMPOSER
    else
        echo UNKNOWN
    fi
}

printf '=== claude pre-grant discriminator ===\n'
printf 'os          : %s %s\n' "$(uname -s)" "$(uname -m)"
printf 'claude      : %s\n' "$("$CLAUDE_BIN" --version 2>&1 | head -1)"
printf 'workspace   : %s\n' "$WORKSPACE"
printf 'settle      : %ss\n\n' "$SETTLE"

declare -a VERDICTS=()
for arm in V0 V1 M1; do
    cfg="$BENCH/cfg-$arm"
    mkdir -p "$cfg" || die "cannot create $cfg"
    case "$arm" in
        V0) : ;;                                   # virgin, nothing granted
        V1) pregrant "$cfg" || die "pre-grant failed for $arm" ;;
        M1) mature "$cfg"; pregrant "$cfg" || die "pre-grant failed for $arm" ;;
    esac
    # Sampled BEFORE the launch. The launch itself matures the directory — that
    # is this bench's own step-1 finding — so reading it afterwards would report
    # `yes` for every arm and quietly destroy the distinction between them.
    matured_before=$(matured_p "$cfg")
    launch_and_capture "$arm" "$cfg" || die "could not launch arm $arm"
    verdict=$(classify "$BENCH/$arm.pane")
    VERDICTS+=("$verdict")
    printf '%-3s  screen=%-18s matured-at-launch=%-4s credential=%s\n' \
        "$arm" "$verdict" "$matured_before" "$(report_credential "$cfg")"
    if [ "$verdict" = UNKNOWN ]; then
        printf '     pane (unrecognised, quoted verbatim):\n'
        sed 's/^/     | /' "$BENCH/$arm.pane"
    fi
done

v0=${VERDICTS[0]}; v1=${VERDICTS[1]}; m1=${VERDICTS[2]}
printf '\n--- verdict ---\n'

# The control first. Three green arms over a bed that was never virgin is the
# most expensive way to be wrong here, so it is checked before anything else.
if [ "$v0" != WIZARD ]; then
    printf 'VOID: the control arm V0 showed %s, not the first-run wizard.\n' "$v0"
    printf 'The bed is not virgin (a CLAUDE_CONFIG_DIR leaked in, or this build\n'
    printf 'does not onboard), so V1 and M1 discriminate nothing. Nothing is concluded.\n'
    exit 1
fi

if [ "$v1" = COMPOSER ]; then
    printf 'MATURATION NOT REQUIRED on this bed: a virgin config dir plus the\n'
    printf 'pre-grant alone reached a composer (V1=%s, M1=%s).\n' "$v1" "$m1"
    printf 'Whatever blocks a spawn here is NOT the un-matured directory.\n'
elif [ "$m1" = COMPOSER ]; then
    printf 'MATURATION REQUIRED on this bed: the pre-grant alone was not enough\n'
    printf '(V1=%s) but a matured directory plus the same pre-grant composed (M1=%s).\n' "$v1" "$m1"
    printf 'This is the result that justifies a maturation step in the spawn path.\n'
else
    printf 'NEITHER arm composed (V1=%s, M1=%s), so this bed is blocked by\n' "$v1" "$m1"
    printf 'something upstream of both the pre-grant and maturation. The panes above\n'
    printf 'name the screen actually reached; that screen is the next thing to chase.\n'
fi
exit 0
