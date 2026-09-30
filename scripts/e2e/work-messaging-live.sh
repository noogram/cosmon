#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Live ADR-182 §3 acceptance. Run from an operator shell after building cs.
# Usage: scripts/e2e/work-messaging-live.sh --out-dir DIR [--cs-bin PATH]

set -uo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel) || exit 2
cs_bin="$repo/target/debug/cs"
out_dir=
while (($#)); do
    case "$1" in
        --out-dir) out_dir=${2:?missing output directory}; shift 2 ;;
        --cs-bin) cs_bin=${2:?missing cs binary}; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[[ -n "$out_dir" && -x "$cs_bin" ]] || {
    echo 'provide --out-dir and an executable --cs-bin' >&2
    exit 2
}
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd -P)
cs_bin=$(cd "$(dirname "$cs_bin")" && pwd -P)/$(basename "$cs_bin")
export PATH="$(dirname "$cs_bin"):$PATH"
rm -f "$out_dir/live.exit"
trap 'status=$?; printf "%s\n" "$status" > "$out_dir/live.exit"' EXIT

# The caller may be a worker shell. Make scratch molecules independent of it.
unset COSMON_MOL_DIR COSMON_PARENT_MOL_ID COSMON_RUNTIME_ACTIVE CB_DEPTH
unset CODEX_SESSION_ID CODEX_THREAD_ID CODEX_CI
while IFS= read -r name; do unset "$name"; done < <(compgen -e | grep '^CLAUDE_CODE_' || true)

report="$out_dir/live-report.md"
if [[ ! -f "$report" ]]; then
    printf '# Work messaging live acceptance\n\n' > "$report"
fi
printf '## Run %s\n\nCLI: `%s`\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$("$cs_bin" --version 2>&1 | head -1)" >> "$report"
failures=0

record_failure() {
    local label=$1 log=$2 detail=$3
    printf -- '- RED %s: %s (log: `%s`)\n' "$label" "$detail" "$log" >> "$report"
    failures=$((failures + 1))
}

new_id() {
    python3 -c 'import json,sys; data=json.load(sys.stdin); print(data.get("id") or data.get("molecule_id") or "")'
}

run_pair() {
    local label=$1 second_adapter=$2 second_model=$3
    local case_dir project log owner a b state first_wait second_wait
    case_dir=$(mktemp -d "$out_dir/${label}.XXXXXX") || return 1
    # cs init refuses a nested galaxy; the durable case log stays in out_dir.
    project=$(mktemp -d "${TMPDIR:-/tmp}/cosmon-work-${label}.XXXXXX") || return 1
    log="$case_dir/run.log"
    printf '\n## %s\n\nScratch galaxy: `%s`\n\n' "$label" "$project" >> "$report"
    (
        cd "$project" || exit 1
        git init -q || exit 1
        "$cs_bin" init --json > "$case_dir/init.json" 2>> "$log" || exit 1
        mkdir -p .cosmon/formulas
        cat > .cosmon/formulas/live-review.formula.toml <<'FORMULA'
formula = "live-review"
version = 1
description = "One bounded review and peer evidence exchange."
id_prefix = "review"

[vars.topic]
description = "The assigned review"
required = true

[[steps]]
id = "exchange"
title = "Exchange evidence"
description = "Review the assigned source and exchange a cited finding."
acceptance = "The peer exchange and consumption reports are recorded."
FORMULA
        cat > alpha.txt <<'ALPHA'
Batch Cedar record
Entries: 10 units and 5 units.
Reported total: 15 units.
ALPHA
        cat > beta.txt <<'BETA'
Batch Cedar audit
Reported total: 14 units.
Source: alpha.txt entries.
BETA
        git add alpha.txt beta.txt .cosmon/formulas/live-review.formula.toml || exit 1
        git commit -qm 'test: seed live messaging fixture' || exit 1
        "$cs_bin" nucleate live-review --var topic='own the peer review' --json > "$case_dir/owner.json" 2>> "$log" || exit 1
        "$cs_bin" nucleate live-review --var topic='review alpha.txt' --json > "$case_dir/a.json" 2>> "$log" || exit 1
        "$cs_bin" nucleate live-review --var topic='review beta.txt' --json > "$case_dir/b.json" 2>> "$log" || exit 1
    ) >> "$log" 2>&1 || {
        record_failure "$label setup" "$log" 'scratch initialization or nucleation failed'
        return 1
    }
    owner=$(new_id < "$case_dir/owner.json")
    a=$(new_id < "$case_dir/a.json")
    b=$(new_id < "$case_dir/b.json")
    if [[ -z "$owner" || -z "$a" || -z "$b" ]]; then
        record_failure "$label setup" "$log" 'nucleation returned an empty ID'
        return 1
    fi
    state="$project/.cosmon/state/fleets/default/molecules"
    cat > "$state/$a/briefing.md" <<BRIEF_A
Live acceptance seat a. Review alpha.txt, especially lines 2-3. Compare the
reported total against beta.txt line 2 as peer evidence; do not change files.
At the first step boundary run cs work inbox. Send exactly one finding to seat b:
cs work send --to b --key alpha-finding --text 'alpha.txt:3 reports 15 units; beta.txt:2 reports 14 for the same Batch Cedar.'
Then check cs work inbox at each step boundary until beta-reply arrives. You may
retry with a short sleep; stop and report a blocker if no reply arrives within
10 minutes. Acknowledge beta-reply using cs work ack beta-reply --considered.
Check cs work inbox once more and complete only after it is empty.
Run cs evolve $a --evidence 'finding sent and reply considered' --formula .cosmon/formulas/live-review.formula.toml.
The one-step formula completes when this evolve succeeds.
No git changes or commits are needed for this evidence-only review.
BRIEF_A
    cat > "$state/$b/briefing.md" <<BRIEF_B
Live acceptance seat b. Review beta.txt, especially line 2, and compare it
with alpha.txt line 3. Do not change files. Run cs work inbox at each
step boundary until alpha-finding arrives. You may retry with a short sleep;
stop and report a blocker if none arrives within 10 minutes.
Acknowledge it with cs work ack alpha-finding --considered and reply:
cs work send --to a --key beta-reply --reply-to alpha-finding --text 'beta.txt:2 says 14 units, conflicting with alpha.txt:3 saying 15; the entries on alpha.txt:2 sum to 15.'
Check cs work inbox once more and complete only after it is empty.
Run cs evolve $b --evidence 'finding considered and cited reply sent' --formula .cosmon/formulas/live-review.formula.toml.
The one-step formula completes when this evolve succeeds.
No git changes or commits are needed for this evidence-only review.
BRIEF_B
    if ! (cd "$project" && "$cs_bin" work declare "$owner" --seat "a=$a" --seat "b=$b") >> "$log" 2>&1; then
        record_failure "$label declare" "$log" 'work scope declaration failed'
        return 1
    fi
    printf 'Owner `%s`; seats `%s`, `%s`. Fixture SHA-256: `%s`, `%s`.\n\n' \
        "$owner" "$a" "$b" \
        "$(shasum -a 256 "$project/alpha.txt" | cut -d' ' -f1)" \
        "$(shasum -a 256 "$project/beta.txt" | cut -d' ' -f1)" >> "$report"
    if ! (cd "$project" && "$cs_bin" tackle "$a" --adapter claude --model claude-sonnet-5-5) >> "$log" 2>&1; then
        record_failure "$label tackle a" "$log" 'Claude dispatch failed'
        return 1
    fi
    if ! (cd "$project" && "$cs_bin" tackle "$b" --adapter "$second_adapter" --model "$second_model") >> "$log" 2>&1; then
        record_failure "$label tackle b" "$log" "$second_adapter dispatch failed"
        return 1
    fi
    (cd "$project" && "$cs_bin" wait "$a" --timeout 1200 --quiet) > "$case_dir/a.wait.log" 2>&1 &
    first_wait=$!
    (cd "$project" && "$cs_bin" wait "$b" --timeout 1200 --quiet) > "$case_dir/b.wait.log" 2>&1 &
    second_wait=$!
    local a_exit=0 b_exit=0
    wait "$first_wait" || a_exit=$?
    wait "$second_wait" || b_exit=$?
    printf 'Wait exits: a=%s, b=%s.\n\n' "$a_exit" "$b_exit" >> "$report"
    if ((a_exit != 0 || b_exit != 0)); then
        record_failure "$label wait" "$case_dir/{a,b}.wait.log" 'one or both members did not reach a terminal state within the bounded wait'
    fi
    if ! (cd "$project" && "$cs_bin" --json work list "$owner") > "$case_dir/work.json" 2>> "$log"; then
        record_failure "$label projection" "$log" 'work list failed'
        return 1
    fi
    local empty_home
    empty_home=$(mktemp -d "$case_dir/empty.XXXXXX")
    if ! (cd "$project" && HOME="$empty_home" CLAUDE_CONFIG_DIR="$empty_home" CODEX_HOME="$empty_home" "$cs_bin" --json work list "$owner") > "$case_dir/work-no-provider-history.json" 2>> "$log"; then
        record_failure "$label recovery" "$log" 'work list failed with empty provider homes'
    elif ! cmp -s "$case_dir/work.json" "$case_dir/work-no-provider-history.json"; then
        record_failure "$label recovery" "$case_dir/work-no-provider-history.json" 'projection changed without provider history'
    else
        printf -- '- GREEN recovery: projection identical with empty provider homes.\n' >> "$report"
    fi
    if python3 - "$case_dir/work.json" "$state" "$a" "$b" <<'PY' >> "$case_dir/assert.log" 2>&1
import json, pathlib, sys
projection = json.loads(pathlib.Path(sys.argv[1]).read_text())
state = pathlib.Path(sys.argv[2])
a, b = sys.argv[3:]
envelopes = projection['envelopes']
assert len(envelopes) >= 2, f'only {len(envelopes)} envelopes'
assert {'alpha-finding', 'beta-reply'} <= envelopes.keys(), envelopes.keys()
assert envelopes['beta-reply']['envelope']['reply_to'] == 'alpha-finding'
for key in ('alpha-finding', 'beta-reply'):
    view = envelopes[key]
    assert view['admitted'], f'{key}: no admission'
    assert view['delivery_attempts'], f'{key}: no delivery attempt'
    assert view['consumed'], f'{key}: no consumption'
    assert view['context_delivered']['observation']['status'] == 'unknown', key
for mid in (a, b):
    molecule = json.loads((state / mid / 'state.json').read_text())
    assert molecule['status'] == 'completed', (mid, molecule.get('status'))
events = [json.loads(line) for line in (state.parents[2] / 'events.jsonl').read_text().splitlines()]
for mid in (a, b):
    rows = [event for event in events if mid in (event.get('molecule_id'), event.get('mol_id'), event.get('molecule'))]
    assert any(event.get('type') == 'molecule_step_completed' for event in rows), mid
    assert any(event.get('type') == 'molecule_completed' for event in rows), mid
    assert not any(event.get('type', '').startswith('work_') for event in rows), mid
print('two cited envelopes, all requested stages, and completed member lifecycles')
PY
    then
        printf -- '- GREEN exchange: two envelopes, reply relation, stages, and member completion asserted.\n' >> "$report"
    else
        record_failure "$label assertions" "$case_dir/assert.log" 'exchange or lifecycle assertion failed'
    fi
}

run_pair mixed codex gpt-5.6-sol || true
run_pair same_claude claude claude-sonnet-5-5 || true
printf '\nFailures: %s. Raw run directories are retained beside this report.\n' "$failures" >> "$report"
((failures == 0))
