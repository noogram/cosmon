#!/usr/bin/env bash
# in-container-bench.sh — replays issue #20's two reported failure scenarios
# against the `cs` built from our corrected branch.
#
# Runs INSIDE docker/container-worker-doors/Dockerfile as root. Each arm
# builds its own throwaway galaxy; nothing here can reach a host fleet.
#
# ── SECRET DISCIPLINE (read this before editing) ───────────────────────
# This script never reads, copies, or requests a real credential. Arms B
# and C need door 3 (the credential gate) to let the dispatch through in
# order to reach the doors BEHIND it, so they mint a PLACEHOLDER
# `.credentials.json` whose token fields are literal refusal strings. It
# is a non-secret by construction: it authenticates nothing, and Claude
# Code renders `Not logged in · Run /login` behind it — which is exactly
# the signal the bench is looking for. Do not "improve" it into a real
# token; that would delete the proof, not strengthen it.
#
# Arms:
#   A  door 3, no credential at all     — the fail-closed refusal
#   B  scenario 1, worktree ownership   — root + COSMON_WORKER_UID=10001
#   C  scenario 2, VIRGIN config dir    — cs demoted via setpriv to 10001
#   D  scenario 2, ONBOARDED config dir — the tester's actual shape
#   E  direct `claude` probes           — cosmon out of the picture
#   F  TWO CONSECUTIVE dispatches       — the 2.1.220 acceptance criterion
#
# C and D differ by one seeded key (`hasCompletedOnboarding`): D inherits it
# from the config, C makes cosmon write it. Since `claude_trust` pre-grants
# onboarding they expect the SAME post-condition — a composer — by two
# different routes, and that convergence is what proves the pre-grant. The
# arms used to expect opposite outcomes, which is why `run_scenario_2` is
# still told which one to grade; see the comment at the C call site for why
# the expectation flipped.
#
# F is the arm the single-dispatch arms cannot replace. Claude Code rewrites
# `.claude.json` from its own in-memory state at exit, so a pre-grant that
# works once may be gone before the next spawn; only two consecutive
# dispatches on one config dir can tell a re-asserted grant from a run-once
# one. Green arms C/D with a red arm F would mean exactly that.
#
# Every arm prints its RAW observations (exit status, stderr, file owner,
# captured pane) and then a machine-greppable verdict line. It never
# asserts-and-dies: a surprising observation must reach the report intact,
# so the script always runs every arm and always exits 0 unless the
# harness itself broke.
set -uo pipefail

say()  { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
sub()  { printf '\033[0;36m  · %s\033[0m\n' "$*"; }
raw()  { printf '\033[0;90m%s\033[0m\n' "$*"; }
hdr()  { printf '\n\033[1;35m═══ %s ═══\033[0m\n' "$*"; }

# Collected verdict lines, replayed at the end for easy transcription.
VERDICTS=()
verdict() { VERDICTS+=("$1"); printf '\n\033[1;33mVERDICT %s\033[0m\n' "$1"; }

# ── 0. Environment fidelity ────────────────────────────────────────────
# These readings are context, NOT a cause. The two sysctls were once treated
# as the reason a namespace creation fails; measured on the tester's own
# container they read 1 and 79654 — healthy — while `unshare -Ur` still
# returned EPERM, because the engine's DEFAULT SECCOMP PROFILE refuses the
# syscall (task-20260726-eabf). So arm 0 prints the sysctls raw (including
# the "unknown key" case, which is NOT a 0), prints the sandbox-policy state
# that actually discriminates, and lets the functional probe decide.
hdr "0. Environment fidelity"
sub "uname -a"
raw "$(uname -a)"
sub "sysctl kernel.unprivileged_userns_clone user.max_user_namespaces"
raw "$(sysctl kernel.unprivileged_userns_clone 2>&1)"
raw "$(sysctl user.max_user_namespaces 2>&1)"
# The sandbox-policy layer: a seccomp filter (Seccomp: 2) or an AppArmor
# profile can refuse `unshare` with every sysctl above permissive. Presence
# is an observation, not an attribution — printed so the report can tell the
# two failure classes apart instead of blaming the sysctls by default.
sub "sandbox policy state: seccomp / AppArmor / SELinux"
raw "$(grep -E '^Seccomp' /proc/self/status 2>&1 || echo '(no Seccomp fields in /proc/self/status)')"
raw "apparmor: $(cat /proc/self/attr/current 2>/dev/null || echo '(no AppArmor interface)')"
raw "selinux:  $(cat /sys/fs/selinux/enforce 2>/dev/null || echo '(no SELinux interface)')"
# A functional probe beats a setting read, and is the ONLY thing here allowed
# to produce a positive claim. Run as the demote target, since that is the
# uid that matters. stderr is captured and reported, never swallowed: the
# real refusal text is what keeps the report from inventing a cause.
sub "functional userns probe, as uid 10001: setpriv --reuid 10001 --regid 10001 --clear-groups unshare -Ur true"
if setpriv --reuid 10001 --regid 10001 --clear-groups \
     unshare -Ur true 2>/tmp/userns.err; then
  raw "unprivileged user namespace creation SUCCEEDS as uid 10001"
else
  raw "unprivileged user namespace creation FAILS as uid 10001: $(cat /tmp/userns.err)"
fi
# Root as well as the demote uid: if BOTH fail, the refusal is not about
# unprivileged-userns policy at all. Reported, not interpreted.
sub "same probe as root: unshare -Ur true"
if unshare -Ur true 2>/tmp/userns-root.err; then
  raw "user namespace creation SUCCEEDS as root"
else
  raw "user namespace creation FAILS as root: $(cat /tmp/userns-root.err)"
fi
sub "cs --version"
raw "$(cs --version 2>&1)"
sub "claude --version"
raw "$(claude --version 2>&1 || echo '(claude --version failed)')"

# PROVENANCE. `cs --version` still says 0.3.0 on our branch, so the version
# string alone cannot distinguish the binary under test from the tag the
# tester ran. These two strings are introduced BY the fixes and exist
# nowhere in v0.3.0 (`cosmon_transport::claude_login` did not exist at that
# tag), so finding them in the shipped binary is what makes every verdict
# below a statement about OUR code rather than about the release.
#
# `awaiting-human` is the door-4 marker and it earns its own line: it is the
# Display form of `SessionStatus::AwaitingHuman`, which exists only after the
# readiness fixes that arm C grades. Without it a red arm C is ambiguous
# between "the fix is absent from this binary" and "the fix is present and
# insufficient" — two findings with nothing in common, and the arm cannot tell
# them apart from its own output.
sub "provenance: fix-only strings that do not exist in v0.3.0"
for marker in "no usable Claude Code credential" "hasTrustDialogAccepted" "awaiting-human"; do
  if grep -aqF "$marker" /usr/local/bin/cs; then
    raw "PRESENT  \"$marker\""
  else
    raw "ABSENT   \"$marker\"  ← the binary under test is NOT the fixed branch"
  fi
done

# ── Helpers ────────────────────────────────────────────────────────────

# mint_placeholder_credential <config-dir> <owner-uid> <seed-onboarding:0|1>
#
# Writes an obviously-invalid `.credentials.json` so door 3 lets the
# dispatch through and the doors behind it become observable. The token
# fields are refusal strings, not redactions of anything: there is no
# secret here to redact. `expiresAt: 0` makes it expired as well as fake.
#
# `seed-onboarding` decides whether `hasCompletedOnboarding` is pre-set.
# It is NOT cosmetic. A genuinely virgin CLAUDE_CONFIG_DIR renders Claude
# Code's first-run THEME WIZARD. The tester's bench was past onboarding —
# he saw the trust dialog, not the wizard — so a faithful replay of his
# scenario 2 must be past it too, which is what arm D seeds.
#
# `claude_trust` now pre-grants that key itself, before every spawn, so an
# UNSEEDED arm is no longer a blocked one: it measures whether cosmon does
# the seeding. That is arm C's whole job since the 2.1.220 report, and arm
# F's across two consecutive dispatches. Seeding it here therefore means
# "hand cosmon a config that is already past onboarding", not "make the
# dispatch possible".
# `hasCompletedOnboarding` is a UI preference, not a credential.
mint_placeholder_credential() {
  local dir="$1" owner="$2" onboard="${3:-0}"
  mkdir -p "$dir"
  cat >"$dir/.credentials.json" <<'PLACEHOLDER'
{
  "claudeAiOauth": {
    "accessToken": "PLACEHOLDER-NOT-A-CREDENTIAL-cosmon-bench-issue-20",
    "refreshToken": "PLACEHOLDER-NOT-A-CREDENTIAL-cosmon-bench-issue-20",
    "expiresAt": 0,
    "scopes": []
  }
}
PLACEHOLDER
  chmod 600 "$dir/.credentials.json"
  if [ "$onboard" = "1" ]; then
    # Left for cosmon's own pre-grant to read-modify-write: it must find a
    # parseable `.claude.json` and add projects[ws].hasTrustDialogAccepted.
    printf '{"hasCompletedOnboarding":true,"projects":{}}' >"$dir/.claude.json"
  fi
  chown -R "$owner:$owner" "$dir"
}

# new_galaxy <path> — the tester's `git init` + empty commit + `cs init`.
new_galaxy() {
  local work="$1"
  mkdir -p "$work"
  cd "$work"
  git config --global --add safe.directory "$work" 2>/dev/null || true
  git init -q
  git config user.name  "cosmon issue-20 bench"
  git config user.email "issue-20-bench@cosmon.invalid"
  git config init.defaultBranch main
  git commit -q --allow-empty -m "empty base commit"
  cs init >/dev/null 2>&1
  git add -A && git commit -qm "cs init" || true
}

# nucleate_one — echoes a molecule id on stdout, diagnostics on stderr.
nucleate_one() {
  cs nucleate task-work --json --var topic="probe the container startup doors" \
    2>/dev/null | jq -r 'select(.id != null) | .id' | grep '^task-' | head -n1
}

# worktree_owner <galaxy> <molecule-id> — `stat -c %u`, or "(absent)".
worktree_owner() {
  local wt="$1/.worktrees/$2"
  if [ -e "$wt" ]; then stat -c %u "$wt"; else echo "(absent)"; fi
}

# ══ ARM A — door 3: no credential anywhere ═════════════════════════════
# Establishes what a credential-less container does NOW. This is also the
# arm that reveals whether fix 3 stands IN FRONT of the scenario-1 repair
# in the dispatch order — if it refuses here, scenario 1 cannot be
# observed without a credential, and that interference is a finding.
hdr "A. Door 3 — cs as root, COSMON_WORKER_UID=10001, NO credential"
A_WORK=/work/arm-a
new_galaxy "$A_WORK"
A_MOL="$(nucleate_one)"
sub "molecule = ${A_MOL:-<none>}"
if [ -z "$A_MOL" ]; then
  verdict "A: HARNESS-BROKEN — cs nucleate produced no molecule id"
else
  # HOME is root's; no credential file exists and no token var is set.
  say "cs tackle $A_MOL --adapter claude   (expect a fail-closed refusal)"
  set +e
  A_OUT="$(cd "$A_WORK" && COSMON_WORKER_UID=10001 \
    timeout 180 cs tackle "$A_MOL" --adapter claude 2>&1)"
  A_RC=$?
  set -e
  sub "exit status = $A_RC"
  sub "raw output:"
  raw "$A_OUT"
  A_OWNER="$(worktree_owner "$A_WORK" "$A_MOL")"
  sub "worktree owner (stat -c %u) = $A_OWNER"
  if printf '%s' "$A_OUT" | grep -qi "credential"; then
    verdict "A: door 3 REFUSED the dispatch (credential named in the refusal), rc=$A_RC, worktree owner=$A_OWNER"
  elif printf '%s' "$A_OUT" | grep -qi "UnprovisionedTarget\|is not usable by it\|chown the worktree"; then
    verdict "A: refused on WORKTREE PROVISIONING, not on the credential, rc=$A_RC, worktree owner=$A_OWNER"
  else
    verdict "A: NEITHER refusal seen — rc=$A_RC, worktree owner=$A_OWNER (read the raw output above)"
  fi
fi

# ══ ARM B — scenario 1: the worktree-ownership catch-22 ════════════════
# The tester's scenario 1. `cs` runs as root and is told to demote to
# 10001. On v0.3.0 this refused with "cannot provision uid 10001:
# worktree … is not usable by it … chown the worktree to the uid before
# tackling", with the worktree owned by 0 — an instruction with no
# "before", since tackle is what creates the worktree.
#
# A placeholder credential is provisioned for uid 10001 ONLY so that door
# 3 (arm A) is not what we measure here. The proof sought is: worktree
# owner == 10001 and no provisioning refusal.
hdr "B. Scenario 1 — cs as root, COSMON_WORKER_UID=10001, placeholder credential"
B_WORK=/work/arm-b
B_CONFIG=/home/cosmon-worker/.claude-arm-b
mint_placeholder_credential "$B_CONFIG" 10001 1
sub "placeholder credential at $B_CONFIG/.credentials.json owned by $(stat -c %u "$B_CONFIG/.credentials.json")"
new_galaxy "$B_WORK"
B_MOL="$(nucleate_one)"
sub "molecule = ${B_MOL:-<none>}"
if [ -z "$B_MOL" ]; then
  verdict "B: HARNESS-BROKEN — cs nucleate produced no molecule id"
else
  # The worktree does not necessarily OUTLIVE the dispatch any more, and
  # that is the door-4 fix working as designed: when the readiness gate
  # refuses, `cleanup_partial_tackle` tears the partial spawn down —
  # session, branch AND worktree — so the molecule returns to `pending`
  # with nothing stranded. A post-hoc `stat` then reads "(absent)" and the
  # arm can prove nothing, which is arm C's problem wearing a third mask:
  # an instrument whose observation point the correct behaviour removes.
  #
  # So observe it WHILE IT EXISTS. This watcher records the first owner the
  # worktree ever has, from the moment tackle creates it. It reads; it never
  # writes, waits on, or perturbs anything cs is doing.
  B_OWNER_SEEN=/tmp/arm-b-owner-observed
  rm -f "$B_OWNER_SEEN"
  (
    wt="$B_WORK/.worktrees/$B_MOL"
    for _ in $(seq 1 1200); do
      if [ -e "$wt" ]; then stat -c %u "$wt" >"$B_OWNER_SEEN" 2>/dev/null; break; fi
      sleep 0.2
    done
  ) &
  B_WATCHER=$!

  say "cs tackle $B_MOL --adapter claude   (expect NO provisioning refusal)"
  set +e
  B_OUT="$(cd "$B_WORK" && COSMON_WORKER_UID=10001 \
    CLAUDE_CONFIG_DIR="$B_CONFIG" \
    HOME=/home/cosmon-worker \
    timeout 240 cs tackle "$B_MOL" --adapter claude 2>&1)"
  B_RC=$?
  kill "$B_WATCHER" 2>/dev/null
  wait "$B_WATCHER" 2>/dev/null
  set -e
  sub "exit status = $B_RC"
  sub "raw output:"
  raw "$B_OUT"
  B_OWNER="$(worktree_owner "$B_WORK" "$B_MOL")"
  B_OWNER_LIVE="$(cat "$B_OWNER_SEEN" 2>/dev/null || echo "(never seen)")"
  sub "worktree owner after the arm (stat -c %u) = $B_OWNER"
  sub "worktree owner WHILE IT EXISTED (watcher) = $B_OWNER_LIVE  (10001 is the proof; 0 is the reported bug)"
  # Prefer the post-hoc reading when there is one — it is the stronger
  # observation, taken on a tree that is still there. Fall back to the
  # watcher's, which is the same `stat` taken earlier in the tree's life.
  if [ "$B_OWNER" = "(absent)" ] && [ "$B_OWNER_LIVE" != "(never seen)" ]; then
    B_OWNER="$B_OWNER_LIVE"
    sub "the worktree was rolled back by the refusal; grading on the watcher's reading"
  fi
  sub "state dir owners:"
  raw "$(find "$B_WORK/.cosmon" -maxdepth 2 -printf '%u  %p\n' 2>/dev/null | head -20)"
  if printf '%s' "$B_OUT" | grep -qi "UnprovisionedTarget\|is not usable by it\|chown the worktree"; then
    verdict "B: NOT PROVEN — the reported provisioning refusal STILL fires, worktree owner=$B_OWNER"
  elif [ "$B_OWNER" = "10001" ]; then
    verdict "B: PROVEN — worktree owned by 10001, no provisioning refusal, rc=$B_RC"
  else
    verdict "B: INCONCLUSIVE — no provisioning refusal but worktree owner=$B_OWNER, rc=$B_RC"
  fi
fi

# ══ ARMS C & D — scenario 2: the doors behind the demotion ═════════════
# The tester's scenario 2. `cs` is launched directly under uid 10001 via
# setpriv, with a HOME and CLAUDE_CONFIG_DIR that uid owns, and NO
# --permission-mode (so the default bypassPermissions applies). He
# observed the preflights passing, the worker starting, and then the pane
# frozen on "Quick safety check: Is this a project you created or one you
# trust?" with the molecule still `running`.
#
# The two arms differ by ONE key and therefore expect TWO DIFFERENT
# post-conditions. That difference is why `run_scenario_2` takes an
# `expect` argument instead of grading both arms against one pane:
#
#   D (onboarded)  expects a COMPOSER.  The discriminant costs no token: a
#      pane showing the composer has passed doors 1 and 2; a pane showing a
#      dialog has not. With only a placeholder credential the composer reads
#      `Not logged in · Run /login` — a PASS for doors 1 and 2, not a
#      failure.
#
#   C (virgin)     expects a REFUSAL.  A virgin config dir parks Claude Code
#      on the login-method selector — a menu waiting for a human. Before the
#      fix, `readiness::classify_output` fell through to the generic
#      last-five-lines `❯` scan, the selector's chevron satisfied it, cosmon
#      called the blocked pane `Ready`, `cs tackle` exited 0 and typed the
#      briefing into a menu. Since the fix an unnamed rendered screen is
#      `AwaitingHuman` → `Liveness::Indeterminate`, so `cs tackle` refuses.
#
#      CORRECT now means there is NO PANE LEFT TO CAPTURE, so grading arm C
#      by grepping a captured pane can only ever say "not conclusive" — an
#      instrument that reports nothing while the build behaves is the same
#      surface lie this issue is about, wearing the other mask. Arm C
#      therefore asserts the refusal's four observable post-conditions:
#        1. `cs tackle` exits NON-ZERO;
#        2. its stderr quotes the login-method selector pane;
#        3. the tmux session is GONE (the carcass was torn down);
#        4. the molecule is NOT left `running`.
#
# One demoted shell does galaxy + nucleate + tackle, so every artefact is
# created by uid 10001 itself — the tester's shape, not a root-created
# tree handed over afterwards.
cat >/tmp/arm-c-inner.sh <<'INNER'
set -uo pipefail
cd "$ARM_C_WORK"
git config --global --add safe.directory "$ARM_C_WORK" 2>/dev/null || true
git init -q
git config user.name  "cosmon issue-20 bench"
git config user.email "issue-20-bench@cosmon.invalid"
git commit -q --allow-empty -m "empty base commit"
cs init >/dev/null 2>&1
git add -A && git commit -qm "cs init" >/dev/null 2>&1
MOL="$(cs nucleate task-work --json --var topic="probe the container startup doors" \
  2>/dev/null | jq -r 'select(.id != null) | .id' | grep '^task-' | head -n1)"
echo "ARM_C_MOL=$MOL"
[ -n "$MOL" ] || exit 90
echo "--- cs tackle (no --permission-mode: default bypassPermissions) ---"
TACKLE_OUT="$(timeout 240 cs tackle "$MOL" --adapter claude 2>&1)"
TACKLE_RC=$?
printf '%s\n' "$TACKLE_OUT"
echo "ARM_C_TACKLE_RC=$TACKLE_RC"

# The pane AT THE INSTANT cs tackle returned, before anything settles.
# Read together with the outer capture 25s later, this separates the two
# ways an exit-0 can be wrong, which no single capture can:
#   same screen twice  → the classifier accepted this screen as a composer;
#   composer then menu → the classifier was right about the frame it saw and
#                        the screen changed under a verdict sampled once.
# cs names its own session either way — `attach:` on success, the `Inspect
# with …` hint on a refusal — so nothing here is guessed.
ATTACH="$(printf '%s\n' "$TACKLE_OUT" | tr -d '`' \
  | grep -o 'tmux -L [^ ]* \(capture-pane -pS - \|attach \)-t [^ ]*' | head -n1)"
if [ -n "$ATTACH" ]; then
  SOCK="$(printf '%s' "$ATTACH" | awk '{print $3}')"
  SESS="$(printf '%s' "$ATTACH" | awk '{print $NF}')"
  echo "--- ARM_C_PANE_AT_RETURN (socket=$SOCK session=$SESS) ---"
  tmux -L "$SOCK" capture-pane -p -t "$SESS" 2>&1
  echo "--- ARM_C_PANE_AT_RETURN_END ---"
fi
INNER
chmod a+rx /tmp/arm-c-inner.sh

# as_worker <cmd...> — run a command as uid 10001 with the arm's HOME.
# Every observation of tmux / cs must be made by the uid that created the
# artefacts, or it reads a different tmux server and a different galaxy.
as_worker() {
  setpriv --reuid 10001 --regid 10001 --clear-groups \
    env HOME=/home/cosmon-worker PATH=/usr/local/bin:/usr/bin:/bin "$@"
}

# run_scenario_2 <arm-letter> <seed-onboarding:0|1> <expect:composer|refusal> <label>
run_scenario_2() {
  local arm="$1" onboard="$2" expect="$3" label="$4"
  local home=/home/cosmon-worker
  local work="$home/arm-$arm" config="$home/.claude-arm-$arm"
  # The readiness trace (COSMON_READINESS_TRACE). Arm C's finding was that the
  # classifier refuses the captured pane while the live dispatch went through
  # it anyway — a contradiction no capture taken from OUTSIDE the process can
  # settle, because it is about what the process saw DURING its window. The
  # trace is that missing observation: one JSON line per sample, each carrying
  # the classified status and the exact bytes classified.
  local trace="$home/readiness-trace-arm-$arm.jsonl"
  local out rc mol attach socket session pane cap_rc
  local tackle_rc refusal_cmd has_rc has_quote has_teardown has_state
  local sess_probe mol_status

  hdr "$(printf '%s' "$arm" | tr a-z A-Z). Scenario 2 — $label"
  mint_placeholder_credential "$config" 10001 "$onboard"
  install -d -o 10001 -g 10001 "$work"
  # A stale trace from a previous arm would be read as this arm's evidence.
  rm -f "$trace"
  sub "HOME=$home (owner $(stat -c %u "$home")), CLAUDE_CONFIG_DIR=$config (owner $(stat -c %u "$config"))"
  sub "config dir contents: $(ls -A "$config" | tr '\n' ' ')"

  say "setpriv --reuid 10001 --regid 10001 --clear-groups  →  cs init / nucleate / tackle"
  set +e
  out="$(setpriv --reuid 10001 --regid 10001 --clear-groups \
    env HOME="$home" \
        CLAUDE_CONFIG_DIR="$config" \
        ARM_C_WORK="$work" \
        COSMON_READINESS_TRACE="$trace" \
        PATH=/usr/local/bin:/usr/bin:/bin \
        bash /tmp/arm-c-inner.sh 2>&1)"
  rc=$?
  set -e
  sub "exit status = $rc"
  sub "raw output:"
  raw "$out"
  mol="$(printf '%s\n' "$out" | sed -n 's/^ARM_C_MOL=//p' | head -n1)"
  # The inner script's own reading of `cs tackle`'s exit status. The OUTER
  # `rc` above is the demoted shell's, and that shell always ends on an
  # `echo`, so it is 0 even when tackle refused. Grading a refusal by the
  # wrong status is how an arm reports silence over a working build.
  tackle_rc="$(printf '%s\n' "$out" | sed -n 's/^ARM_C_TACKLE_RC=//p' | head -n1)"
  sub "cs tackle exit status = ${tackle_rc:-<unread>}"

  # ── What the readiness loop actually observed ────────────────────────
  # Printed BEFORE any verdict, because it is the observation the verdict
  # is about. Two projections, both bounded so the report stays readable:
  #   1. every sample as one line — when, which event, which status, which
  #      collapsed liveness, how many lines the capture carried;
  #   2. the FULL captured bytes for the first sample of each distinct
  #      status, which is what lets a reader diff the screen the probe
  #      classified against the screen the bench captures further down.
  if [ -s "$trace" ]; then
    hdr "$arm. readiness trace — what the process observed during its window"
    sub "samples (elapsed_ms / event / status / liveness / pane_lines / note):"
    raw "$(jq -r '[((.elapsed_ms // "-")|tostring), .event, (.status // "-"), (.liveness // "-"), ((.pane_lines // "-")|tostring), (.note // "-")] | @tsv' "$trace" 2>&1)"
    sub "the exact bytes classified — first sample of each distinct status:"
    raw "$(jq -s -r 'map(select(.event=="capture" and .pane != null))
                     | group_by(.status) | map(.[0]) | .[]
                     | "───── classified \(.status) at \(.elapsed_ms // "?")ms, \(.pane_lines) lines ─────\n\(.pane)"' \
             "$trace" 2>&1)"
  else
    sub "readiness trace: EMPTY or absent at $trace (the loop wrote nothing)"
  fi

  # What cosmon wrote into the config as its pre-grant — the two keys the
  # fix claims to set. Read with jq so no token field can be echoed.
  if [ -f "$config/.claude.json" ]; then
    sub "post-tackle pre-grant state (.claude.json projects map):"
    raw "$(jq -c '{hasCompletedOnboarding, projects: (.projects | with_entries(.value |= {hasTrustDialogAccepted}))}' \
             "$config/.claude.json" 2>&1)"
  fi
  if [ -f "$config/settings.json" ]; then
    sub "post-tackle pre-grant state (settings.json):"
    raw "$(jq -c '{skipDangerousModePermissionPrompt}' "$config/settings.json" 2>&1)"
  fi

  if [ "$expect" = "refusal" ]; then
    # ── The four post-conditions of a CORRECT refusal ────────────────────
    # The verdict is NOT read off a pane. Since the fix the correct
    # behaviour leaves no pane to read, so a pane-shaped instrument can only
    # answer "not conclusive" over a build that worked. What is graded
    # instead is the refusal itself, in the four ways it is observable from
    # outside the process. A pane is still CAPTURED when one survives — that
    # is the failing case, and a bench that grades a live pane without
    # showing it reproduces the silence it exists to break.
    hdr "$arm. refusal post-conditions"

    # 1 — non-zero exit. An exit of 0 here IS the pre-fix pathology
    #     verbatim: the briefing typed into a menu, the operator told yes.
    if [ -n "$tackle_rc" ] && [ "$tackle_rc" != "0" ]; then
      has_rc=yes
    else
      has_rc=no
    fi
    sub "1. cs tackle exited non-zero: $has_rc (rc=${tackle_rc:-<unread>})"

    # 2 — the refusal quotes the screen it refused. `Pane showed: …` is
    #     what makes this a diagnosis instead of an invitation to re-run.
    #     Anchored ON that prefix, not on the screen's words anywhere in
    #     the output: the arm prints the pane itself further down, and a
    #     check that matched that would credit cs for a sentence the bench
    #     wrote.
    #
    #     WHICH screen, and why this line changed. It used to demand the
    #     LOGIN-METHOD SELECTOR, on the reasonable belief that the selector
    #     was what blocked the dispatch. The instrumented run of 2026-07-25
    #     showed that belief was wrong: for the whole 30 s readiness window
    #     the pane was the FIRST-RUN THEME WIZARD (`Let's get started.`),
    #     and the selector only appeared AFTERWARDS, when the briefing cs
    #     typed into the wizard answered it. Every capture the bench took
    #     was post-return, which is why it only ever saw the second screen.
    #
    #     So a correct refusal quotes the wizard, not the selector — and an
    #     arm that insisted on the selector would now report a failure over
    #     a build behaving exactly as intended. Both are accepted, because
    #     both are real doors and which one is on screen at 30 s is a fact
    #     about Claude Code's onboarding order, not about cosmon. What is
    #     NOT loosened: the quote must still be there, still anchored on
    #     cs's own prefix, and still name an onboarding screen by its words.
    if printf '%s' "$out" | grep -q "Pane showed:.*\(Select login method\|Let's get started\|Choose the text style\)"; then
      has_quote=yes
    else
      has_quote=no
    fi
    sub "2. stderr cites the blocking onboarding pane (wizard or selector): $has_quote"

    # 3 — no carcass. The Indeterminate path calls maybe_terminate before
    #     returning. cs names its session either way — via `attach:` when it
    #     believes it succeeded, via the refusal's `Inspect with tmux -L … -t
    #     …` hint when it declines — so read BOTH forms and probe the session
    #     cs itself named rather than one the bench guessed. The `|| true` is
    #     load-bearing: an earlier arm leaves errexit armed, and a grep that
    #     correctly finds nothing must not kill the report.
    refusal_cmd="$(printf '%s\n' "$out" | tr -d '`' \
      | grep -o 'tmux -L [^ ]* \(capture-pane -pS - \|attach \)-t [^ ]*' \
      | head -n1 || true)"
    socket="$(printf '%s' "$refusal_cmd" | awk '{print $3}')"
    session="$(printf '%s' "$refusal_cmd" | awk '{print $NF}')"
    if [ -z "$session" ]; then
      has_teardown=unknown
      sess_probe="(neither a refusal nor a success named a session to probe)"
    else
      set +e
      sess_probe="$(as_worker tmux -L "$socket" has-session -t "$session" 2>&1)"
      if [ $? -eq 0 ]; then has_teardown=no; else has_teardown=yes; fi
      set -e
    fi
    sub "3. tmux session torn down: $has_teardown (socket=${socket:-<none>} session=${session:-<none>}); has-session said: ${sess_probe:-<silent>}"

    # A session that outlived the refusal IS the finding. Let it settle the
    # same 25s arm D allows, then print it — the pane's own words are what
    # turn "not proven" into a diagnosis.
    pane=""
    if [ "$has_teardown" = no ]; then
      sub "a session survived — settling 25s, then capturing what is on it"
      sleep 25
      set +e
      pane="$(as_worker tmux -L "$socket" capture-pane -p -t "$session" 2>&1)"
      cap_rc=$?
      set -e
      hdr "$arm. the pane cs tackle left ALIVE (socket=$socket session=$session rc=$cap_rc)"
      raw "$pane"
    fi

    # 4 — the molecule must not be parked `running` behind a refusal. That
    #     stuck row, alive on a pane blocked forever, is the whole fault.
    if [ -n "${mol:-}" ]; then
      set +e
      mol_status="$(cd "$work" && as_worker cs observe "$mol" --json 2>/dev/null \
        | jq -r '.status // empty')"
      set -e
      [ -n "$mol_status" ] || mol_status="(unreadable)"
    else
      mol_status="(no molecule id)"
    fi
    case "$mol_status" in
      running | Running)                  has_state=no ;;
      "(unreadable)" | "(no molecule id)") has_state=unknown ;;
      *)                                  has_state=yes ;;
    esac
    sub "4. molecule not left running: $has_state (status=$mol_status)"

    if [ "$has_rc" = yes ] && [ "$has_quote" = yes ] \
      && [ "$has_teardown" = yes ] && [ "$has_state" = yes ]; then
      verdict "$arm: PROVEN — cs tackle REFUSED the blocking onboarding pane: rc=$tackle_rc, stderr quotes the screen it refused, tmux session $session gone, molecule=$mol_status"
    elif [ "$has_teardown" = no ] && printf '%s' "$pane" | grep -qi "Select login method\|Choose the text style"; then
      verdict "$arm: NOT PROVEN — an ONBOARDING SCREEN is still certified alive: cs tackle exited ${tackle_rc:-<unread>} over it, the session $session is still up, molecule=$mol_status (the pre-fix pathology, unchanged)"
    elif [ "$has_teardown" = no ]; then
      verdict "$arm: NOT PROVEN — cs tackle exited ${tackle_rc:-<unread>} and LEFT SESSION $session ALIVE; molecule=$mol_status; the surviving pane is captured above (it is NOT the login selector — the arm's premise may no longer hold)"
    else
      verdict "$arm: NOT PROVEN — refusal post-conditions [exit=$has_rc quote=$has_quote teardown=$has_teardown molecule-state=$has_state], rc=${tackle_rc:-<unread>}, molecule=$mol_status (read the raw output above)"
    fi
  # Let the TUI settle, then read the pane. What is on it IS the verdict.
  elif printf '%s' "$out" | grep -q "attach:.*tmux -L"; then
    attach="$(printf '%s\n' "$out" | grep -o 'tmux -L [^ ]* attach -t [^ ]*' | head -n1)"
    socket="$(printf '%s' "$attach" | awk '{print $3}')"
    session="$(printf '%s' "$attach" | awk '{print $6}')"
    sub "tmux socket=$socket session=$session — settling 25s before capture"
    sleep 25
    set +e
    pane="$(as_worker tmux -L "$socket" capture-pane -p -t "$session" 2>&1)"
    cap_rc=$?
    set -e
    hdr "$arm. captured pane (socket=$socket session=$session rc=$cap_rc)"
    raw "$pane"
    # Order matters: the theme wizard also contains the menu chevron, so it
    # must be classified BEFORE any generic composer match — the same
    # ordering constraint readiness::detect_status documents.
    if printf '%s' "$pane" | grep -qi "Select login method"; then
      # An onboarded config dir is not supposed to reach this screen at all,
      # and since the fix a session parked on it is not supposed to survive
      # `cs tackle` either. Seeing it here means BOTH: the arm's own premise
      # (`hasCompletedOnboarding`) did not hold, and the refusal did not
      # fire. Report it as an anomaly, not as a door being named.
      verdict "$arm: ANOMALY — the LOGIN-METHOD SELECTOR is on the pane of an ONBOARDED config dir, and cs tackle left it alive"
    elif printf '%s' "$pane" | grep -qi "Choose the text style\|Let's get started"; then
      verdict "$arm: ANOMALY — the FIRST-RUN THEME WIZARD is on the pane of an ONBOARDED config dir, and cs tackle left it alive"
    elif printf '%s' "$pane" | grep -qi "Is this a project you created\|Quick safety check\|Yes, I trust this folder"; then
      verdict "$arm: NOT PROVEN — the folder-TRUST DIALOG is still on the pane"
    elif printf '%s' "$pane" | grep -qi "Bypass Permissions mode\|Yes, I accept"; then
      verdict "$arm: NOT PROVEN — the BYPASS DISCLAIMER is still on the pane"
    elif printf '%s' "$pane" | grep -qi "bypass permissions on\|Not logged in\|/login\|shift+tab to cycle"; then
      verdict "$arm: PROVEN — the pane reached the COMPOSER (no startup dialog); doors 1 and 2 passed"
    else
      verdict "$arm: INCONCLUSIVE — pane matched neither a dialog nor the composer (read the capture above)"
    fi
  else
    hdr "$arm. no tmux pane was created"
    if printf '%s' "$out" | grep -qi "credential"; then
      verdict "$arm: NOT EXECUTABLE — door 3 refused before any pane existed (the two fixes interfere)"
    else
      verdict "$arm: NOT EXECUTABLE — cs tackle created no pane; see the raw output above"
    fi
  fi

  if [ -n "${mol:-}" ]; then
    sub "molecule state after the arm:"
    raw "$(cd "$work" && as_worker cs observe "$mol" 2>&1 | head -20)"
  fi
}

# C — virgin config dir. THE arm whose expectation the 2.1.220 report flipped.
#
# It used to expect a REFUSAL, and that was right for the code it graded: a
# virgin config dir opened on the first-run theme wizard, cosmon did not
# pre-grant onboarding, and the correct behaviour was to decline the dispatch
# loudly. The tester confirmed that refusal firing on his bench — a good
# failure, and still a dispatch that did not happen.
#
# `claude_trust` now pre-grants `hasCompletedOnboarding` alongside folder trust,
# so a virgin config dir is no longer a blocked one: the wizard never renders and
# the worker reaches its composer. Expecting a refusal here would now report red
# over the fix working. C and D therefore expect the SAME outcome by different
# routes — D inherits the key from a seeded config, C has cosmon write it — and
# that convergence is the proof.
#
# The refusal path is not left untested by the flip: it is what any UNNAMED
# screen still gets (§8v), it is pinned by the readiness suite, and arm F below
# exercises the pre-grant across two consecutive dispatches, which is the only
# shape that can catch a grant that works once and then evaporates.
run_scenario_2 c 0 composer "cs demoted via setpriv to 10001, VIRGIN config dir — cosmon must pre-grant onboarding itself"

# D — onboarded config dir. THE faithful replay of the tester's scenario 2:
# he saw the trust dialog, so his bench was necessarily past onboarding.
run_scenario_2 d 1 composer "cs demoted via setpriv to 10001, ONBOARDED config dir — the tester's shape"

# ══ ARM E — what the placeholder itself does, with cosmon out of the way ═
# Arms C/D observe a pane through `cs tackle`, which means two variables
# move at once: cosmon's pre-grant, and the placeholder credential. If a
# login selector appears, that is not yet attributable.
#
# So this arm drives `claude` DIRECTLY — the guide's own by-hand recipe
# (claude-worker-in-a-container.md, "Verifying by hand") — across a small
# factorial, with cosmon absent from the picture entirely:
#
#   E1  onboarded, NO credential file      ← the guide's documented claim
#   E2  onboarded, placeholder credential  ← is the bogus file the cause?
#
# Both have folder trust and the bypass disclaimer pre-granted by hand, so
# doors 1 and 2 cannot be what shows up. Whatever appears is door 3's real
# shape on this Claude Code build, measured rather than inherited.
hdr "E. Direct claude probes — isolating what the placeholder credential causes"
probe_claude_direct() {
  local tag="$1" seed_cred="$2" cfg ws pane
  cfg="/home/cosmon-worker/.claude-$tag"
  ws="/home/cosmon-worker/ws-$tag"
  install -d -o 10001 -g 10001 "$ws"
  mkdir -p "$cfg"
  # Both consent gates granted by hand — trust keyed on the exact workspace.
  jq -n --arg ws "$ws" \
    '{hasCompletedOnboarding:true, projects:{($ws):{hasTrustDialogAccepted:true}}}' \
    >"$cfg/.claude.json"
  printf '{"skipDangerousModePermissionPrompt":true}' >"$cfg/settings.json"
  if [ "$seed_cred" = "1" ]; then
    mint_placeholder_credential "$cfg" 10001 1
    jq -n --arg ws "$ws" \
      '{hasCompletedOnboarding:true, projects:{($ws):{hasTrustDialogAccepted:true}}}' \
      >"$cfg/.claude.json"
    printf '{"skipDangerousModePermissionPrompt":true}' >"$cfg/settings.json"
  fi
  chown -R 10001:10001 "$cfg"
  sub "$tag: config dir contents: $(ls -A "$cfg" | tr '\n' ' ')"
  setpriv --reuid 10001 --regid 10001 --clear-groups \
    env HOME=/home/cosmon-worker PATH=/usr/local/bin:/usr/bin:/bin \
    tmux -L "probe-$tag" new-session -d -x 200 -y 50 -c "$ws" \
      "CLAUDE_CONFIG_DIR=$cfg claude --permission-mode bypassPermissions" 2>&1 || true
  sleep 25
  set +e
  pane="$(setpriv --reuid 10001 --regid 10001 --clear-groups \
    env HOME=/home/cosmon-worker PATH=/usr/local/bin:/usr/bin:/bin \
    tmux -L "probe-$tag" capture-pane -p 2>&1)"
  set -e
  hdr "E. pane for $tag"
  raw "$pane"
  setpriv --reuid 10001 --regid 10001 --clear-groups \
    env HOME=/home/cosmon-worker PATH=/usr/local/bin:/usr/bin:/bin \
    tmux -L "probe-$tag" kill-server 2>/dev/null || true
  if printf '%s' "$pane" | grep -qi "Select login method"; then
    verdict "$tag: LOGIN SELECTOR (a blocking dialog)"
  elif printf '%s' "$pane" | grep -qi "Choose the text style\|Let's get started"; then
    verdict "$tag: first-run THEME WIZARD"
  elif printf '%s' "$pane" | grep -qi "Not logged in\|/login"; then
    verdict "$tag: COMPOSER with the 'Not logged in' footer (the documented shape)"
  elif printf '%s' "$pane" | grep -qi "bypass permissions on\|shift+tab to cycle"; then
    verdict "$tag: COMPOSER, authenticated-looking (no footer)"
  else
    verdict "$tag: UNRECOGNISED pane (read the capture above)"
  fi
}
probe_claude_direct e1-onboarded-no-cred 0
probe_claude_direct e2-onboarded-placeholder-cred 1

# ══ ARM F — TWO CONSECUTIVE dispatches on ONE pristine config dir ══════
# The acceptance criterion of the 2.1.220 report, and the only arm shaped
# to catch the failure it describes.
#
# Claude Code rewrites `.claude.json` wholesale from its own in-memory
# state when a session ends, dropping keys the running build does not
# recognise. Which keys survive is version- and state-dependent: measured
# on 2.1.220 the onboarding and trust keys survived, while the tester
# measured the onboarding key going to `null` across the same cycle on his
# bench. So a pre-grant is NOT storage, and a design that writes it once
# at image-build time passes dispatch 1 and fails dispatch 2.
#
# Every other arm here dispatches once and therefore cannot tell the two
# designs apart. This one dispatches twice into the same config dir with
# NOTHING in between — no operator, no re-seed — and lets the first worker
# be torn down before the second starts, so the config rewrite actually
# happens. Both panes must be composers. `Not logged in · Run /login` is
# expected on both: the placeholder credential authenticates nothing, and a
# composer is exactly the proof sought — no dialog stood in front of it.
hdr "F. Two CONSECUTIVE dispatches, one pristine config dir, nothing in between"
F_HOME=/home/cosmon-worker
F_WORK="$F_HOME/arm-f"
F_CONFIG="$F_HOME/.claude-arm-f"
# seed-onboarding=0: the directory must be genuinely virgin, so that what
# is being graded is cosmon's own re-assertion and not a seeded key.
mint_placeholder_credential "$F_CONFIG" 10001 0
install -d -o 10001 -g 10001 "$F_WORK"

# f_dispatch <n> — runs one dispatch and records its verdict.
# Called directly, never in a subshell: `verdict` appends to VERDICTS, and a
# subshell would drop the arm from the summary replay while still printing it.
f_dispatch() {
  local n="$1" out pane socket session attach
  sub "F$n: pre-grant state BEFORE the dispatch:"
  if [ -f "$F_CONFIG/.claude.json" ]; then
    raw "$(jq -c '{hasCompletedOnboarding, projects: (.projects | with_entries(.value |= {hasTrustDialogAccepted}))}' \
             "$F_CONFIG/.claude.json" 2>&1)"
  else
    raw "(.claude.json absent — the virgin case)"
  fi

  set +e
  out="$(setpriv --reuid 10001 --regid 10001 --clear-groups \
    env HOME="$F_HOME" CLAUDE_CONFIG_DIR="$F_CONFIG" ARM_C_WORK="$F_WORK" \
        PATH=/usr/local/bin:/usr/bin:/bin \
        bash /tmp/arm-c-inner.sh 2>&1)"
  set -e
  sub "F$n: raw output:"
  raw "$out"

  attach="$(printf '%s\n' "$out" | tr -d '`' \
    | grep -o 'tmux -L [^ ]* \(capture-pane -pS - \|attach \)-t [^ ]*' | head -n1 || true)"
  socket="$(printf '%s' "$attach" | awk '{print $3}')"
  session="$(printf '%s' "$attach" | awk '{print $NF}')"
  if [ -z "$session" ]; then
    verdict "F$n: NOT EXECUTABLE — no session was named (read the raw output above)"
    return
  fi
  sleep 25
  set +e
  pane="$(as_worker tmux -L "$socket" capture-pane -p -t "$session" 2>&1)"
  set -e
  hdr "F$n. captured pane (socket=$socket session=$session)"
  raw "$pane"

  # Tear the worker down before the next dispatch. This is load-bearing:
  # the config rewrite happens when the session ENDS, and a dispatch 2 that
  # ran while dispatch 1 was still alive would never meet the rewrite it
  # exists to survive.
  as_worker tmux -L "$socket" kill-server 2>/dev/null || true
  sleep 5

  if printf '%s' "$pane" | grep -qi "Choose the text style\|Let's get started"; then
    verdict "F$n: FAILED — the first-run THEME WIZARD is on the pane (the pre-grant did not hold for this dispatch)"
  elif printf '%s' "$pane" | grep -qi "Select login method"; then
    verdict "F$n: FAILED — the LOGIN-METHOD SELECTOR is on the pane"
  elif printf '%s' "$pane" | grep -qi "Quick safety check\|Yes, I trust this folder"; then
    verdict "F$n: FAILED — the folder-TRUST DIALOG is on the pane"
  elif printf '%s' "$pane" | grep -qi "Bypass Permissions mode\|Yes, I accept"; then
    verdict "F$n: FAILED — the BYPASS DISCLAIMER is on the pane"
  elif printf '%s' "$pane" | grep -qi "bypass permissions on\|Not logged in\|/login\|shift+tab to cycle"; then
    verdict "F$n: PASSED — the pane reached the COMPOSER, no startup dialog"
  else
    verdict "F$n: INCONCLUSIVE — pane matched neither a dialog nor the composer (read the capture above)"
  fi
}

f_dispatch 1
sub "F: pre-grant state AFTER dispatch 1 exited — what dispatch 2 inherits:"
raw "$(jq -c '{hasCompletedOnboarding, projects: (.projects | with_entries(.value |= {hasTrustDialogAccepted}))}' \
         "$F_CONFIG/.claude.json" 2>&1)"
f_dispatch 2

# ── Replay ─────────────────────────────────────────────────────────────
hdr "VERDICT SUMMARY"
for v in "${VERDICTS[@]}"; do printf 'VERDICT %s\n' "$v"; done
exit 0
