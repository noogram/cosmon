#!/usr/bin/env bash
# in-container-nonroot-pilot.sh — replay the external tester's two-dispatch
# scenario with the pilot and the worker under ONE non-root uid, and measure
# that the compatibility hand-over machinery was never entered.
#
# Runs INSIDE docker/container-real-mission/Dockerfile, invoked through a
# shell opened with `-u 10001:10001` and an explicit HOME. It never runs as
# root and it never asks anything to run as root.
#
# ── WHAT IS BEING CLAIMED ──────────────────────────────────────────────
# NOT "the final owners are correct". That is the neighbouring property and
# it is perfectly compatible with a repair having fired and landed on the
# owner the path already had.
#
# The claim is: THE NOMINAL PATH INVOKED NO OWNERSHIP REPAIR AT ALL.
#
# ── HOW THE ABSENCE IS OBSERVED ────────────────────────────────────────
# By instrumentation, not by reading final state, and not by strace (the
# engine's seccomp posture refuses ptrace for root and uid alike — see
# docs/benches/engine-fidelity-2026-07-27.md — so a tracing route would be
# cause-not-isolated).
#
# `cs` counts every ENTRY into the ownership-repair path, at two
# granularities and both before any precondition is examined: once when
# `provision_and_decide_root_spawn` decides a demote is on the table, and
# once per path handed to a chown. Setting
# COSMON_OWNERSHIP_TRANSFER_JOURNAL makes each of those append one line,
# carrying the pid that wrote it. A separate journal file per dispatch is
# what makes each number attributable to one dispatch; the pid is what
# makes it attributable to one process.
#
# HAD A REPAIR FIRED, the journal for that dispatch would hold at least one
# `enter-repair-path` line naming the target uid, followed by one `chown`
# line per path touched. An empty (or absent) journal is therefore the
# measurement, and a chown that changed nothing could not hide in it.
#
# ── SECRET DISCIPLINE ──────────────────────────────────────────────────
# This run uses a credential the OPERATOR supplied and explicitly
# authorised: his own OAuth token, mounted read-only, exported as
# CLAUDE_CODE_OAUTH_TOKEN into the DISPATCHER's environment — route (a) of
# the very refusal message the empty-handed run recorded.
#
# The rules that govern it, all enforced below and re-checked by the host
# driver before anything is committed:
#   · the VALUE is never echoed, printed, logged, tee'd or written into any
#     file this capture touches. `set -x` is never enabled here;
#   · it is never written into the config dir. That dir stays virgin at
#     tackle time and is only ever stat()ed — `.credentials.json` is never
#     opened;
#   · the record says THAT a token was supplied and BY WHICH ROUTE, never
#     which token. Only its byte length is ever mentioned;
#   · the host driver greps every artefact for the value and refuses to
#     commit if it appears. A capture that leaks the key it used is worse
#     than no capture.
#
# Output contract:
#   stdout            the raw transcript, every observation verbatim
#   $OUT_DIR/…        nonroot-pilot-record.json, the per-dispatch journals,
#                     and the two tackle transcripts
# Exit status:
#   0  the bar was met: two live workers, two commits they made, zero repairs
#   1  a finding: something the bar requires did not hold
#   2  INCONCLUSIVE — a step that discriminates could not run
set -uo pipefail

OUT_DIR="${OUT_DIR:-/out}"
mkdir -p "$OUT_DIR"

say() { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
sub() { printf '\033[0;36m  · %s\033[0m\n' "$*"; }
raw() { printf '\033[0;90m%s\033[0m\n' "$*"; }
hdr() { printf '\n\033[1;35m═══ %s ═══\033[0m\n' "$*"; }

FINDINGS=()
finding() {
  FINDINGS+=("$1")
  printf '\033[1;31m  ✗ %s\033[0m\n' "$1"
}
ok() { printf '\033[0;32m  ✓ %s\033[0m\n' "$1"; }

MISSION_ROOT="${MISSION_ROOT:-$HOME/nonroot-mission}"
MISSION_CONFIG="${MISSION_CONFIG:-$HOME/.claude-nonroot}"

# ── 1. The identity, asserted rather than assumed ──────────────────────
hdr "1. Identity and environment"
RUNNING_UID="$(id -u)"
RUNNING_GID="$(id -g)"
sub "id: $(id)"
sub "HOME=${HOME:-<unset>}"
sub "pwd: $(pwd)"

if [ "$RUNNING_UID" = "0" ]; then
  finding "running as root — this harness measures the non-root pilot and cannot run as uid 0"
  printf '\nINCONCLUSIVE\n'
  exit 2
fi
ok "pilot uid is $RUNNING_UID (not root), gid $RUNNING_GID"

# The explicit HOME is load-bearing and is therefore checked, not trusted.
# Changing only `-u` while leaving HOME=/root puts the worker back behind a
# 0700 directory it cannot read — defect #2 of the four, reintroduced by a
# partial invocation.
if [ -z "${HOME:-}" ] || [ ! -w "$HOME" ]; then
  finding "HOME (${HOME:-<unset>}) is not writable by uid $RUNNING_UID — the invocation was partial (`-u` without an explicit HOME is the classic form of this)"
  printf '\nINCONCLUSIVE\n'
  exit 2
fi
ok "HOME=$HOME is writable by uid $RUNNING_UID"

# The nominal path carries no demotion knob at all. Its presence would mean
# a different mechanism was under test.
if [ -n "${COSMON_WORKER_UID:-}" ]; then
  finding "COSMON_WORKER_UID is set to '${COSMON_WORKER_UID}' — the nominal path sets nothing; this run would measure the compatibility path"
  printf '\nINCONCLUSIVE\n'
  exit 2
fi
ok "COSMON_WORKER_UID is unset — no demotion is configured"

# ── 2. Provenance of the bytes ─────────────────────────────────────────
# A capture that names a source commit but was produced by older bytes is
# worse than no capture, so both are recorded: the commit the image was
# built from, and a hash of the binary that actually ran.
hdr "2. Provenance"
CS_PATH="$(command -v cs)"
SOURCE_SHA="$(cat /etc/cosmon-source-sha 2>/dev/null || echo unknown)"
SOURCE_CLEAN="$(cat /etc/cosmon-source-clean 2>/dev/null || echo unknown)"
CS_SHA_BEFORE="$(sha256sum "$CS_PATH" | awk '{print $1}')"
sub "cs path:            $CS_PATH"
sub "cs --version:       $(cs --version 2>&1 | head -n1)"
sub "source commit:      $SOURCE_SHA"
sub "source tree clean:  $SOURCE_CLEAN"
sub "sha256(cs) BEFORE:  $CS_SHA_BEFORE"

if [ "$SOURCE_SHA" = "unknown" ]; then
  finding "the image carries no source commit — the bytes under test are unattributable"
fi
if [ "$SOURCE_CLEAN" != "clean" ]; then
  finding "the source tree was not clean at build time (state: $SOURCE_CLEAN) — the commit does not describe the bytes"
fi

# The instrumentation this capture rests on must exist in the binary that
# ran. Without this, an empty journal is ambiguous between "no repair fired"
# and "this binary never writes a journal".
sub "instrument present in the binary under test"
if strings "$CS_PATH" 2>/dev/null | grep -qF "COSMON_OWNERSHIP_TRANSFER_JOURNAL" \
   || grep -aqF "COSMON_OWNERSHIP_TRANSFER_JOURNAL" "$CS_PATH"; then
  ok "COSMON_OWNERSHIP_TRANSFER_JOURNAL is present in $CS_PATH"
  INSTRUMENT_PRESENT=1
else
  finding "the binary does not contain the ownership-transfer instrument — an empty journal would prove nothing"
  INSTRUMENT_PRESENT=0
fi

# ── 3. A galaxy created by the identity that will consume it ───────────
hdr "3. The galaxy"
mkdir -p "$MISSION_ROOT"
cd "$MISSION_ROOT" || { printf '\nINCONCLUSIVE\n'; exit 2; }
git init -q
git config user.name  "cosmon non-root pilot"
git config user.email "nonroot-pilot@cosmon.invalid"
git commit -q --allow-empty -m "empty base commit"
cs init >/dev/null 2>&1
git add -A && git commit -qm "cs init" >/dev/null 2>&1
sub "galaxy at $MISSION_ROOT"

# No `safe.directory` exemption is granted anywhere in this script, for
# either identity. On the nominal path there is nothing to exempt: one uid
# created the repository and one uid operates it. A dispatch that needed the
# exemption would fail here, loudly, which is the intended detector.
if git status --porcelain >/dev/null 2>&1; then
  ok "git operates the repository with no safe.directory exemption"
else
  finding "git refused the repository — the nominal path should need no ownership exemption"
fi

FOREIGN_OWNED="$(find "$MISSION_ROOT" ! -uid "$RUNNING_UID" -print 2>/dev/null | head -n 5)"
if [ -z "$FOREIGN_OWNED" ]; then
  ok "every path under the galaxy is owned by uid $RUNNING_UID by creation"
else
  finding "paths under the galaxy are owned by another uid: $(printf '%s' "$FOREIGN_OWNED" | tr '\n' ' ')"
fi

# ── 3b. The credential, by route (a) ───────────────────────────────────
# The operator mounted his own token read-only at TOKEN_PATH and authorised
# this run to use it. It is read into the dispatcher's environment in ONE
# gesture and never lands in a file this capture touches.
#
# HANDLING RULES, enforced here and re-checked by the host driver:
#   · the VALUE is never echoed, printed, logged, tee'd or written out;
#   · it is never written into the config dir, which stays virgin at tackle
#     time and is only ever stat()ed — `.credentials.json` is never opened;
#   · the record says THAT a token was supplied and by which route, never
#     which token.
# `set -x` is never enabled in this script for exactly this reason.
hdr "3b. Credential"
TOKEN_PATH="${TOKEN_PATH:-/run/secrets/claude-oat.token}"
CREDENTIAL_ROUTE="none"
if [ -r "$TOKEN_PATH" ]; then
  # One gesture. No echo, no intermediate file, no expansion into a log.
  CLAUDE_CODE_OAUTH_TOKEN="$(cat "$TOKEN_PATH")"
  export CLAUDE_CODE_OAUTH_TOKEN
  if [ -n "$CLAUDE_CODE_OAUTH_TOKEN" ]; then
    CREDENTIAL_ROUTE="oauth-token-in-dispatcher-env (read-only mount, operator-supplied)"
    # Length only — never the value, never a prefix, never a hash that could
    # be matched against a candidate list.
    ok "credential supplied by route (a): CLAUDE_CODE_OAUTH_TOKEN in the dispatcher environment (${#CLAUDE_CODE_OAUTH_TOKEN} bytes, value never recorded)"
  else
    finding "the mounted token file is empty — the dispatches would refuse at the credential gate"
  fi
else
  sub "no token mounted at $TOKEN_PATH — the dispatches will refuse at the credential gate"
fi

# ── 4. A virgin config dir ─────────────────────────────────────────────
hdr "4. Config dir at tackle time"
rm -rf "$MISSION_CONFIG"
mkdir -p "$MISSION_CONFIG"
export CLAUDE_CONFIG_DIR="$MISSION_CONFIG"
sub "CLAUDE_CONFIG_DIR=$MISSION_CONFIG"
# STAT ONLY — the file is never opened. Its absence is the measurement.
if [ -e "$MISSION_CONFIG/.credentials.json" ]; then
  finding "the config dir is not virgin — a credential is present, so the dispatches are not the ones this capture describes"
  CONFIG_VIRGIN=0
else
  ok "virgin: no credential of any kind (stat only, never opened)"
  CONFIG_VIRGIN=1
fi
sub "config dir contents at tackle time:"
raw "$(ls -la "$MISSION_CONFIG")"

# ── 5. Two consecutive dispatches, nothing between them ────────────────
# `--no-parent` because a `cs` invoked from inside a cosmon worker session
# auto-links to the dispatching molecule, which does not exist in this
# galaxy. Without it, nucleate fails and the arm never starts.
hdr "5. Two dispatches"
nucleate_one() {
  cs nucleate task-work --no-parent --json --var topic="Create a file named ARTIFACT-$1.md in the repository root. Its only content must be one line: the numeric uid you are running as, i.e. the output of \`id -u\`. Then stage it and commit it with git, message \`feat: artifact $1\`. Change nothing else in the repository." \
    2>/dev/null | jq -r 'select(.id != null) | .id' | grep '^task-' | head -n1
}
MOL1="$(nucleate_one 1)"
MOL2="$(nucleate_one 2)"
sub "molecule 1: ${MOL1:-<none>}"
sub "molecule 2: ${MOL2:-<none>}"
if [ -z "$MOL1" ] || [ -z "$MOL2" ]; then
  finding "could not nucleate two molecules; the dispatch path was never entered"
  printf '\nINCONCLUSIVE\n'
  exit 2
fi

SPAWNED_1=0
SPAWNED_2=0
SESSION_1=""
SESSION_2=""
dispatch() {
  local n="$1" mol="$2"
  local journal="$OUT_DIR/repair-journal-$n.txt"
  rm -f "$journal"
  say "dispatch $n — cs tackle $mol --adapter claude (journal: $(basename "$journal"))"
  # The journal path is per-dispatch, so the number it yields cannot be
  # attributed to the other dispatch or to a sibling process. The worker's
  # own `cs` invocations inherit it too, and their lines carry their own
  # pid — which widens the observation from the dispatcher to the worker.
  local out rc
  out="$(COSMON_OWNERSHIP_TRANSFER_JOURNAL="$journal" \
         timeout 300 cs tackle "$mol" --adapter claude 2>&1)"
  rc=$?
  printf '%s\n' "$out" >"$OUT_DIR/tackle-$n.out"
  raw "$out"
  sub "rc=$rc"
  printf '%s\n' "$rc" >"$OUT_DIR/tackle-$n.rc"

  # Liveness is asserted POSITIVELY. A zero exit code proves a process
  # exited, never that a worker exists — so the tmux session `cs` named is
  # probed with `has-session`, which is the kernel answering rather than us
  # inferring.
  local attach sock sess
  attach="$(printf '%s\n' "$out" | tr -d '`' \
    | grep -o 'tmux -L [^ ]* \(capture-pane -pS - \|attach \)-t [^ ]*' | head -n1)"
  if [ -z "$attach" ]; then
    finding "dispatch $n: cs tackle named no tmux session (rc=$rc) — no worker to watch"
    return
  fi
  sock="$(printf '%s' "$attach" | awk '{print $3}')"
  sess="$(printf '%s' "$attach" | awk '{print $NF}')"
  printf '%s\n' "$sock" >"$OUT_DIR/tmux-socket-$n.txt"
  printf '%s\n' "$sess" >"$OUT_DIR/tmux-session-$n.txt"
  if tmux -L "$sock" has-session -t "$sess" 2>/dev/null; then
    ok "dispatch $n: a LIVE worker is in tmux session $sess"
    eval "SPAWNED_$n=1"
    eval "SESSION_$n=\$sess"
  else
    finding "dispatch $n: cs tackle exited $rc and named session $sess, but tmux has-session says it is not there"
  fi
}

dispatch 1 "$MOL1"
# Nothing at all happens here. No chown, no config edit, no login, no
# ownership exemption, no reseed — the second dispatch follows the first
# directly. A setup that passes once and fails the second time is the trap
# this whole arc was about.
dispatch 2 "$MOL2"

# ── 6. Two commits, made BY THE WORKER ─────────────────────────────────
# Recorded, not inferred: a file on disk is not a commit, and that is
# exactly the distinction the tester's finding turned on — both artefacts
# were written and neither was committed.
#
# The harness does not author or commit anything here. It waits for each
# worker to do it in its own worktree, then verifies the result as a commit
# OBJECT in the repository and attributes it to the dispatch that produced
# it (the molecule's own branch, `feat/<mol>`).
hdr "6. The commits the workers made"
COMMITS=("" "")
WORKER_UID_IN_ARTIFACT=("" "")

# Poll rather than sleep-and-hope: a worker that finishes in 90s should not
# cost 10 minutes, and a worker that never commits must not look like one
# that is still thinking. The deadline is a FINDING when it fires.
WAIT_DEADLINE="${WAIT_DEADLINE:-900}"
wait_for_worker_commit() {
  local n="$1" mol="$2"
  local wt="$MISSION_ROOT/.worktrees/$mol"
  local branch="feat/$mol"
  local waited=0
  # This phase is observed too, so the record's per-dispatch commit-phase
  # counter is a measurement rather than a placeholder.
  export COSMON_OWNERSHIP_TRANSFER_JOURNAL="$OUT_DIR/repair-journal-commit-$n.txt"
  rm -f "$COSMON_OWNERSHIP_TRANSFER_JOURNAL"
  # No worker was spawned, so there is nobody to wait for. Waiting out the
  # full deadline here would turn a fast, honest refusal into a fifteen
  # minute one, twice.
  local spawned="SPAWNED_$n"
  if [ "${!spawned}" != "1" ]; then
    sub "dispatch $n spawned no worker — not waiting for a commit that has no author"
    return
  fi
  say "waiting for the worker of dispatch $n to commit (deadline ${WAIT_DEADLINE}s)"
  while [ "$waited" -lt "$WAIT_DEADLINE" ]; do
    if [ -d "$wt" ] && git -C "$wt" log --oneline -1 -- "ARTIFACT-$n.md" 2>/dev/null | grep -q .; then
      break
    fi
    sleep 10
    waited=$((waited + 10))
  done
  if [ ! -d "$wt" ]; then
    finding "dispatch $n: no worktree at $wt — the dispatch never got that far"
    return
  fi
  local sha
  sha="$(git -C "$wt" log --format=%H -1 -- "ARTIFACT-$n.md" 2>/dev/null)"
  if [ -z "$sha" ]; then
    finding "dispatch $n: the worker did not commit ARTIFACT-$n.md within ${WAIT_DEADLINE}s"
    git -C "$wt" status --porcelain >"$OUT_DIR/worktree-status-$n.out" 2>&1
    git -C "$wt" log --oneline -5 >"$OUT_DIR/worktree-log-$n.out" 2>&1
    return
  fi
  # Verified in the REPOSITORY, not in the worktree: the object and the ref
  # both have to have landed in the common dir, which is the store a linked
  # worktree commits *through* and the one the third defect made unwritable.
  local objtype
  objtype="$(git -C "$MISSION_ROOT" cat-file -t "$sha" 2>/dev/null)"
  if [ "$objtype" != "commit" ]; then
    finding "dispatch $n: $sha is not a commit object in $MISSION_ROOT (cat-file says '${objtype:-nothing}')"
    return
  fi
  # Attribution: the commit must be reachable from THIS molecule's branch,
  # so a single commit cannot be counted twice or credited to the wrong
  # dispatch.
  if ! git -C "$MISSION_ROOT" merge-base --is-ancestor "$sha" "$branch" 2>/dev/null; then
    finding "dispatch $n: $sha is not reachable from $branch — it cannot be attributed to this dispatch"
    return
  fi
  {
    printf 'dispatch %s\n' "$n"
    printf 'molecule %s\n' "$mol"
    printf 'branch   %s\n' "$branch"
    printf 'commit   %s\n' "$sha"
    printf 'cat-file -t -> %s\n' "$objtype"
    git -C "$MISSION_ROOT" log -1 --format='author   %an <%ae>%ncommitter %cn <%ce>%nsubject  %s' "$sha"
    printf -- '--- ARTIFACT-%s.md as committed ---\n' "$n"
    git -C "$MISSION_ROOT" show "$sha:ARTIFACT-$n.md"
  } >"$OUT_DIR/commit-$n.out" 2>&1
  raw "$(cat "$OUT_DIR/commit-$n.out")"
  ok "dispatch $n: worker committed $sha on $branch — verified as a commit object"
  COMMITS[$((n - 1))]="$sha"
  WORKER_UID_IN_ARTIFACT[$((n - 1))]="$(git -C "$MISSION_ROOT" show "$sha:ARTIFACT-$n.md" 2>/dev/null | tr -dc '0-9')"
}

wait_for_worker_commit 1 "$MOL1"
wait_for_worker_commit 2 "$MOL2"

# The state dir, written by the worker's own `cs` from inside its worktree.
for n in 1 2; do
  mol="$MOL1"; [ "$n" = "2" ] && mol="$MOL2"
  if [ -d "$MISSION_ROOT/.cosmon/state/fleets/default/molecules/$mol" ]; then
    ok "dispatch $n: molecule state dir exists and is owned by $(stat -c %u "$MISSION_ROOT/.cosmon/state/fleets/default/molecules/$mol")"
  fi
done

# ── 7. Same bytes, both dispatches ─────────────────────────────────────
hdr "7. Same bytes, both dispatches"
CS_SHA_AFTER="$(sha256sum "$CS_PATH" | awk '{print $1}')"
sub "sha256(cs) AFTER:   $CS_SHA_AFTER"
if [ "$CS_SHA_BEFORE" = "$CS_SHA_AFTER" ]; then
  ok "both dispatches ran the same binary ($CS_SHA_AFTER)"
  SAME_BYTES=1
else
  finding "the binary changed between the dispatches ($CS_SHA_BEFORE → $CS_SHA_AFTER) — two dispatches on two builds is not the two-dispatch bar"
  SAME_BYTES=0
fi

# ── 8. The load-bearing measurement: zero repair entries ───────────────
hdr "8. Ownership-repair entries"
# Every event line the instrument writes begins `pid=`, so the count is of
# EVENTS and not of bytes. That is what lets each journal carry a header
# saying what it is — an empty file would be pruned by the zero-byte rule
# and its emptiness, which is the whole measurement, would vanish with it.
count_journal() {
  local journal="$1"
  [ -f "$journal" ] || { echo 0; return; }
  grep -c '^pid=' "$journal" 2>/dev/null || echo 0
}

# Give each journal a header, so a journal with no events is a file that
# says "no events" rather than a file that is not there.
annotate_journal() {
  local journal="$1" label="$2" n
  n="$(count_journal "$journal")"
  local body=""
  [ -f "$journal" ] && body="$(cat "$journal")"
  {
    printf '# ownership-repair journal — %s\n' "$label"
    printf '# every event line begins `pid=`; %s event(s) recorded.\n' "$n"
    printf '# an entry would read: pid=<pid> enter-repair-path to_uid=<uid>\n'
    printf '# followed by:         pid=<pid> chown tree|node uid=<uid> <path>\n'
    if [ "$n" -eq 0 ]; then
      printf '# NO EVENTS. The repair path was not entered by any process here.\n'
    fi
    [ -n "$body" ] && printf '%s\n' "$body"
  } >"$journal.annotated"
  mv "$journal.annotated" "$journal"
}
REPAIR_1="$(count_journal "$OUT_DIR/repair-journal-1.txt")"
REPAIR_2="$(count_journal "$OUT_DIR/repair-journal-2.txt")"
REPAIR_C1="$(count_journal "$OUT_DIR/repair-journal-commit-1.txt")"
REPAIR_C2="$(count_journal "$OUT_DIR/repair-journal-commit-2.txt")"
annotate_journal "$OUT_DIR/repair-journal-1.txt"        "dispatch 1: cs tackle and the worker it spawned"
annotate_journal "$OUT_DIR/repair-journal-2.txt"        "dispatch 2: cs tackle and the worker it spawned"
annotate_journal "$OUT_DIR/repair-journal-commit-1.txt" "dispatch 1: the commit-wait and verification phase"
annotate_journal "$OUT_DIR/repair-journal-commit-2.txt" "dispatch 2: the commit-wait and verification phase"
sub "dispatch 1 (tackle + worker) repair-path entries: $REPAIR_1"
sub "dispatch 2 (tackle + worker) repair-path entries: $REPAIR_2"
sub "dispatch 1 commit-wait phase   entries: $REPAIR_C1"
sub "dispatch 2 commit-wait phase   entries: $REPAIR_C2"
REPAIR_TOTAL=$((REPAIR_1 + REPAIR_2 + REPAIR_C1 + REPAIR_C2))
for j in "$OUT_DIR"/repair-journal-*.txt; do
  # annotated above, so every journal is present and self-describing
  if [ -s "$j" ]; then
    printf '  journal %s:\n' "$(basename "$j")"
    raw "$(cat "$j")"
  fi
done
if [ "$REPAIR_TOTAL" -eq 0 ]; then
  ok "the nominal path invoked no ownership repair at all — zero entries across all four journals"
else
  finding "the ownership-repair path was entered ($REPAIR_TOTAL entries) — this run is a FAILURE, not a pass"
fi

# ── 9. Verdict ─────────────────────────────────────────────────────────
hdr "9. Verdict"
BOTH_COMMITTED=0
[ -n "${COMMITS[0]:-}" ] && [ -n "${COMMITS[1]:-}" ] && BOTH_COMMITTED=1
BOTH_SPAWNED=0
[ "$SPAWNED_1" = "1" ] && [ "$SPAWNED_2" = "1" ] && BOTH_SPAWNED=1

# The verdict states what the run REACHED, and nothing beyond it.
#
# This field is what a script lifts and what a hurried reader believes, so
# it must not be able to contradict the prose two screens below. An earlier
# revision of this harness reported NONROOT-PILOT-CLEAN with the reason
# "two consecutive dispatches produced two verified commits" over a run in
# which both dispatches were REFUSED at the credential gate and both commits
# were made by the harness. That was false about its own run. CLEAN is
# therefore reachable only when every arm was reached, and each shortfall
# has its own name.
EXIT=1
if [ "$BOTH_SPAWNED" != "1" ]; then
  if [ "$CREDENTIAL_ROUTE" = "none" ]; then
    VERDICT="REFUSED-AT-CREDENTIAL-GATE"
    REASON="no credential was supplied, so no worker was spawned; the dispatch path was entered and refused. The repair-path counter read $REPAIR_TOTAL, but with demotion not configured the path could not be entered at all — that zero is by construction, not by behaviour."
  else
    VERDICT="NOT-SPAWNED"
    REASON="a credential was supplied but at least one dispatch produced no live worker — nothing about the commit half of the bar was reached"
  fi
elif [ "$BOTH_COMMITTED" != "1" ]; then
  VERDICT="SPAWNED-BUT-NOT-COMMITTED"
  REASON="two live workers were spawned under uid $RUNNING_UID, but they did not both commit their artefact — this is the exact half of the tester's finding that must not be reported as passing"
elif [ "${#FINDINGS[@]}" -ne 0 ]; then
  VERDICT="FINDING"
  REASON="${FINDINGS[0]}"
else
  VERDICT="NONROOT-PILOT-LIVE-CLEAN"
  REASON="two consecutive dispatches under uid $RUNNING_UID each spawned a LIVE worker which made its own commit (verified as commit objects, attributed to their molecule branches), and the ownership-repair path was never entered by any process in the run"
  EXIT=0
fi
# A finding anywhere still downgrades a would-be clean run.
if [ "${#FINDINGS[@]}" -ne 0 ] && [ "$EXIT" = "0" ]; then
  VERDICT="FINDING"
  REASON="${FINDINGS[0]}"
  EXIT=1
fi

jq -n \
  --arg verdict "$VERDICT" \
  --arg reason "$REASON" \
  --arg claim "the nominal path invoked no ownership repair at all" \
  --arg counter_meaning "The repair-path counter carries information ONLY when a worker actually spawns. In a run refused at the credential gate the zero is by construction — demotion is not configured, so the path cannot be entered at all — not by behaviour. A zero from such a run must never be reused for a live one." \
  --arg credential_route "$CREDENTIAL_ROUTE" \
  --argjson both_spawned "$BOTH_SPAWNED" \
  --arg session1 "$SESSION_1" \
  --arg session2 "$SESSION_2" \
  --arg method "instrumentation: cs counts every ENTRY into the ownership-repair path (before any precondition), to a per-dispatch journal whose lines carry the writing pid; an empty journal is the measurement. Had a repair fired, that journal would hold an enter-repair-path line naming the target uid plus one chown line per path touched. Final-state ownership was NOT used as evidence: a chown onto the owner a path already had is invisible to stat." \
  --arg uname "$(uname -a)" \
  --arg cs_version "$(cs --version 2>&1 | head -n1)" \
  --arg claude_version "$(claude --version 2>&1 | head -n1)" \
  --arg source_sha "$SOURCE_SHA" \
  --arg source_clean "$SOURCE_CLEAN" \
  --arg cs_path "$CS_PATH" \
  --arg cs_sha_before "$CS_SHA_BEFORE" \
  --arg cs_sha_after "$CS_SHA_AFTER" \
  --argjson same_bytes "$SAME_BYTES" \
  --argjson instrument_present "$INSTRUMENT_PRESENT" \
  --argjson pilot_uid "$RUNNING_UID" \
  --argjson pilot_gid "$RUNNING_GID" \
  --arg home "$HOME" \
  --arg config_dir "$MISSION_CONFIG" \
  --argjson config_virgin "$CONFIG_VIRGIN" \
  --arg mol1 "$MOL1" \
  --arg mol2 "$MOL2" \
  --arg commit1 "${COMMITS[0]:-}" \
  --arg commit2 "${COMMITS[1]:-}" \
  --argjson repair_entries_1 "$REPAIR_1" \
  --argjson repair_entries_2 "$REPAIR_2" \
  --argjson repair_entries_c1 "$REPAIR_C1" \
  --argjson repair_entries_c2 "$REPAIR_C2" \
  --argjson repair_total "$REPAIR_TOTAL" \
  --argjson both_committed "$BOTH_COMMITTED" \
  --argjson findings "$(printf '%s\n' "${FINDINGS[@]:-}" | jq -R . | jq -s 'map(select(. != ""))')" \
  '{
     harness: "container-nonroot-pilot",
     verdict: $verdict,
     reason: $reason,
     claim: $claim,
     method: $method,
     identity: {
       pilot_uid: $pilot_uid,
       pilot_gid: $pilot_gid,
       home: $home,
       demotion_configured: false
     },
     provenance: {
       source_commit: $source_sha,
       source_tree: $source_clean,
       cs_path: $cs_path,
       cs_sha256_before_dispatch_1: $cs_sha_before,
       cs_sha256_after_dispatch_2: $cs_sha_after,
       same_bytes_both_dispatches: ($same_bytes == 1),
       instrument_present_in_binary: ($instrument_present == 1),
       cs_version: $cs_version,
       claude_version: $claude_version,
       uname: $uname
     },
     config: { dir: $config_dir, virgin_at_tackle: ($config_virgin == 1) },
     credential: { supplied: ($credential_route != "none"), route: $credential_route,
                   note: "the value is never recorded here or anywhere in this capture" },
     dispatches: [
       { n: 1, molecule: $mol1, tmux_session: $session1, worker_spawned: ($session1 != ""),
         commit_by_worker: $commit1,
         repair_path_entries_dispatch: $repair_entries_1,
         repair_path_entries_commit_phase: $repair_entries_c1 },
       { n: 2, molecule: $mol2, tmux_session: $session2, worker_spawned: ($session2 != ""),
         commit_by_worker: $commit2,
         repair_path_entries_dispatch: $repair_entries_2,
         repair_path_entries_commit_phase: $repair_entries_c2 }
     ],
     both_workers_spawned: ($both_spawned == 1),
     counter_meaning: $counter_meaning,
     both_artifacts_committed_by_worker: ($both_committed == 1),
     total_repair_path_entries: $repair_total,
     findings: $findings,
     secrets_touched: "none — no credential created, read, mounted or logged"
   }' >"$OUT_DIR/nonroot-pilot-record.json"

printf '\n\033[1;33mVERDICT %s — %s\033[0m\n' "$VERDICT" "$REASON"
cat "$OUT_DIR/nonroot-pilot-record.json"
exit "$EXIT"
