#!/usr/bin/env bash
# whisper-to-spark-tick.sh — consume admitted whispers from
# `.cosmon/whispers/inbox/<room>/*.md`, nucleate one `spark` molecule per
# whisper via `cs nucleate spark`, archive the processed whisper file to
# `.cosmon/whispers/sparked/<room>/<fname>`.
#
# This is the mechanical half of the formula `whisper-to-spark`: idempotent,
# LLM-free, safe to fire every N minutes via LaunchAgent. The formula exists
# as a manual `cs nucleate whisper-to-spark` entry point over the same
# mechanism (see `.cosmon/formulas/whisper-to-spark.formula.toml`).
#
# Idempotence: the whisper filename (`<ts>-<event_id>.md`) is the dedup key.
# If a file with the same name already lives under `.cosmon/whispers/sparked/`
# it is skipped and no new spark is emitted.
#
# Flags:
#   --cosmon-root <DIR>   explicit project root containing `.cosmon/`.
#                         defaults to walk-up from cwd.
#   --max <N>             cap sparks emitted this tick (default: 50)
#   --json                emit NDJSON on stdout (one object per whisper
#                         + one `tick_complete` summary line at EOF).
#                         default: human-readable.
#   --help                show this help.
#
# Exit codes:
#   0 — success (including zero whispers in inbox)
#   1 — operator error (bad flags, no .cosmon/ root found)
#   2 — transient failure (nucleate failed, mv failed) — launchd will
#       log it and re-fire at the next interval.
#
# Invariants respected:
#   - §8j ingress: we do NOT re-do admission; we consume files already
#     admitted by matrix-echo-tick.
#   - ADR-016 no-daemon-in-core: this is a one-shot script fired by
#     an external scheduler (LaunchAgent).
#   - CLI-first for workers: we invoke `cs nucleate spark` (not MCP).

set -euo pipefail

SELF_NAME="whisper-to-spark"
MAX_EVENTS=50
JSON_MODE=0
COSMON_ROOT=""

usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "${SELF_NAME}: $*" >&2
    exit 1
}

emit_json() {
    # Emit one compact JSON object. Keys are passed as key=value pairs.
    local first=1
    printf '{'
    while (($#)); do
        local pair="$1"; shift
        local k="${pair%%=*}"
        local v="${pair#*=}"
        if (( first )); then first=0; else printf ','; fi
        # Escape the value for JSON: backslash + double-quote + newline.
        local esc="${v//\\/\\\\}"
        esc="${esc//\"/\\\"}"
        esc="${esc//$'\n'/\\n}"
        printf '"%s":"%s"' "$k" "$esc"
    done
    printf '}\n'
}

# Walk up from $1 until a directory containing `.cosmon/whispers/` is found.
# Echoes the root, or empty string if not found.
discover_root() {
    local dir="$1"
    dir="$(cd "$dir" && pwd -P)"
    while [[ "$dir" != "/" && -n "$dir" ]]; do
        if [[ -d "$dir/.cosmon/whispers/inbox" ]]; then
            printf '%s\n' "$dir"
            return 0
        fi
        dir="$(dirname "$dir")"
    done
    printf ''
}

# Extract a YAML scalar value from a frontmatter block. Strips surrounding
# quotes. Returns empty string if key absent. Reads stdin.
yaml_get() {
    local key="$1"
    awk -v k="$key" '
        BEGIN { in_fm = 0; hits = 0 }
        /^---[[:space:]]*$/ {
            if (in_fm == 0) { in_fm = 1; next }
            else { exit }
        }
        in_fm == 1 {
            # match "key: value"  — value can be optionally double-quoted.
            n = index($0, ":")
            if (n == 0) next
            tag = substr($0, 1, n - 1)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", tag)
            if (tag != k) next
            val = substr($0, n + 1)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", val)
            # strip optional wrapping double-quotes.
            if (length(val) >= 2 && substr(val, 1, 1) == "\"" && substr(val, length(val), 1) == "\"") {
                val = substr(val, 2, length(val) - 2)
            }
            print val
            hits++
            exit
        }
    ' "$2"
}

# Extract the body (everything after the second "---" frontmatter fence).
yaml_body() {
    awk '
        BEGIN { in_fm = 0; past_fm = 0 }
        past_fm == 1 { print; next }
        /^---[[:space:]]*$/ {
            if (in_fm == 0) { in_fm = 1; next }
            else { past_fm = 1; next }
        }
    ' "$1"
}

# Parse CLI flags.
while (($#)); do
    case "$1" in
        --cosmon-root) COSMON_ROOT="$2"; shift 2 ;;
        --max) MAX_EVENTS="$2"; shift 2 ;;
        --json) JSON_MODE=1; shift ;;
        -h|--help|help) usage 0 ;;
        *) echo "${SELF_NAME}: unknown flag: $1" >&2; usage 1 ;;
    esac
done

if [[ -z "$COSMON_ROOT" ]]; then
    COSMON_ROOT="$(discover_root "$(pwd)")"
    [[ -n "$COSMON_ROOT" ]] || die "no .cosmon/whispers/inbox/ found above $(pwd) — pass --cosmon-root <DIR>"
fi

[[ -d "$COSMON_ROOT/.cosmon/whispers/inbox" ]] || die "$COSMON_ROOT/.cosmon/whispers/inbox does not exist"

INBOX="$COSMON_ROOT/.cosmon/whispers/inbox"
SPARKED="$COSMON_ROOT/.cosmon/whispers/sparked"
mkdir -p "$SPARKED"

command -v cs >/dev/null 2>&1 || die "'cs' not on PATH"

found=0
sparked=0
skipped=0
failed=0

# Iterate every room directory under inbox/.
shopt -s nullglob
for room_dir in "$INBOX"/*/; do
    [[ -d "$room_dir" ]] || continue
    room_name="$(basename "$room_dir")"
    mkdir -p "$SPARKED/$room_name"

    for whisper in "$room_dir"*.md; do
        [[ -f "$whisper" ]] || continue
        (( found++ )) || true
        fname="$(basename "$whisper")"
        archived="$SPARKED/$room_name/$fname"

        # Idempotence — the event_id-derived filename is the dedup key.
        if [[ -e "$archived" ]]; then
            (( skipped++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=whisper_skipped" \
                    "reason=already_sparked" \
                    "path=$whisper"
            else
                echo "${SELF_NAME}: skip (already sparked): $fname"
            fi
            continue
        fi

        if (( sparked >= MAX_EVENTS )); then
            break 2
        fi

        # Parse the frontmatter fields we care about.
        event_id="$(yaml_get event_id "$whisper")"
        sender_nucleon_id="$(yaml_get sender_nucleon_id "$whisper")"
        room_id="$(yaml_get room_id "$whisper")"
        body="$(yaml_body "$whisper")"
        # Trim trailing blank lines but keep the intent verbatim.
        while [[ -n "$body" && "${body: -1}" == $'\n' ]]; do body="${body%?}"; done

        if [[ -z "$body" ]]; then
            (( failed++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=whisper_rejected" \
                    "reason=empty_body" \
                    "path=$whisper"
            else
                echo "${SELF_NAME}: reject (empty body): $fname" >&2
            fi
            continue
        fi

        # Build `cs nucleate spark` invocation. Extra vars (whisper_source,
        # matrix_event_id, room_id) pass through the nucleate layer
        # (see crates/cosmon-core/src/nucleate.rs — "Include user-supplied
        # variables not declared in the formula"). They land in prompt.md's
        # frontmatter `variables:` block via the usual rendering path.
        #
        # Tag values must satisfy cosmon's tag validator: no whitespace,
        # no `:`, 1..=64 printable ASCII. Matrix event IDs begin with `$`
        # and may exceed 64 chars on some homeservers, so we sanitize:
        # strip the leading `$`, replace `:` with `_`, truncate to 64.
        # The full untruncated event id remains available via the
        # `matrix_event_id` variable (in `prompt.md`).
        eid_tag="${event_id#\$}"
        eid_tag="${eid_tag//:/_}"
        eid_tag="${eid_tag:0:64}"
        args=(
            nucleate spark
            --var "topic=$body"
            --var "nucleon_id=$sender_nucleon_id"
            --var "whisper_source=$whisper"
            --var "matrix_event_id=$event_id"
            --var "room_id=$room_id"
            --tag temp:hot
            --tag source:whisper
            --tag stream:matrix
            --tag "matrix-event:$eid_tag"
            --no-parent
        )

        # Invoke cs. We use `--json` so we can parse the resulting spark id
        # deterministically. cs is the walk-up-discovering CLI; no fleet
        # flag needed beyond the default.
        if ! out="$(cd "$COSMON_ROOT" && cs --json "${args[@]}" 2>&1)"; then
            (( failed++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=whisper_failed" \
                    "reason=nucleate_error" \
                    "path=$whisper" \
                    "stderr=$out"
            else
                echo "${SELF_NAME}: nucleate failed for $fname: $out" >&2
            fi
            continue
        fi

        # Best-effort extraction of the spark id from the JSON output.
        # `cs --json nucleate` emits a single-line JSON object with keys
        # in alphabetical order. Match the exact `"id":"<value>"` pair
        # via a regex and take what sits between the inner pair of
        # quotes; avoids misfires on sibling keys like `formula_id`,
        # `assigned_worker`, or stderr noise from the `auto-linked to
        # parent` informational line (suppressed by `--no-parent`).
        spark_id="$(printf '%s' "$out" \
            | grep -oE '"id"[[:space:]]*:[[:space:]]*"[^"]+"' \
            | head -1 \
            | sed -E 's/^.*:[[:space:]]*"([^"]+)".*$/\1/')"

        # Move the whisper to sparked/<room>/. If the rename fails (disk
        # full, permissions), we leave a breadcrumb — the next tick will
        # detect the duplicate via the `matrix:event:<eid>` tag check.
        if ! mv "$whisper" "$archived"; then
            (( failed++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=whisper_archive_failed" \
                    "path=$whisper" \
                    "spark_id=$spark_id"
            else
                echo "${SELF_NAME}: mv failed for $fname after spark $spark_id" >&2
            fi
            continue
        fi

        (( sparked++ )) || true
        if (( JSON_MODE )); then
            emit_json \
                "event=spark_created" \
                "spark_id=$spark_id" \
                "event_id=$event_id" \
                "sender_nucleon_id=$sender_nucleon_id" \
                "path=$archived"
        else
            echo "${SELF_NAME}: sparked $spark_id from $fname (sender=$sender_nucleon_id)"
        fi
    done
done

if (( JSON_MODE )); then
    emit_json \
        "event=tick_complete" \
        "found=$found" \
        "sparked=$sparked" \
        "skipped=$skipped" \
        "failed=$failed"
else
    echo "${SELF_NAME}: tick_complete — found=$found sparked=$sparked skipped=$skipped failed=$failed"
fi

if (( failed > 0 )); then
    exit 2
fi
exit 0
