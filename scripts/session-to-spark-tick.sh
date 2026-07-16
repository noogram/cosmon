#!/usr/bin/env bash
# session-to-spark-tick.sh — convert session notes to spark molecules.
#
# Sibling of `whisper-to-spark-tick.sh`. Where the whisper tick consumes
# Matrix-ingressed whispers (`.cosmon/whispers/inbox/<room>/*.md`), this
# tick consumes operator session notes from
# `.cosmon/state/sessions/session-*.md` and nucleates one `spark` molecule
# per qualifying note.
#
# Two selection modes:
#   1. **Prefix syntactic**: any note body that begins with `!spark ` is
#      auto-promoted (the prefix is stripped before the body becomes the
#      spark topic).
#   2. **Explicit list** (`--promote-notes <session_id>:<ts>,...`): the
#      operator names specific notes regardless of prefix. Used by
#      `cs session promote <note_ts>`.
#
# Idempotence: the session is sealed (BLAKE3, §8b) and MUST NOT be
# rewritten. Promotion markers live in a **sidecar** directory:
#
#     .cosmon/state/sessions/.promoted/<session_id>/<note_ts>.md
#
# Presence of the sidecar = "this note was already promoted, skip".
# The sidecar body records the spark molecule id and the note body so a
# later audit can verify the chain without re-parsing the session file.
#
# Flags:
#   --cosmon-root <DIR>     explicit project root containing `.cosmon/`.
#                           defaults to walk-up from cwd.
#   --session <ID|PATH>     process only a single session file. default:
#                           every `session-*.md` under sessions/.
#   --promote-notes <LIST>  comma-separated list of `<session_id>@<ts>`
#                           pairs. promote these notes regardless of the
#                           `!spark ` prefix. `<session_id>` may be the
#                           file stem (`session-2026-04-23T10-00-00Z`)
#                           or omitted (then applies to the session named
#                           by `--session`). `@` was chosen as the
#                           separator because note timestamps contain
#                           colons (`HH:MM:SS`).
#   --all-spark-prefix      promote every note whose body starts with
#                           `!spark `. this is the default behaviour,
#                           the flag exists so the LaunchAgent invocation
#                           can be explicit and self-documenting.
#   --dry-run               print what would be promoted without
#                           nucleating or writing sidecars.
#   --max <N>               cap sparks emitted this tick (default: 50).
#   --json                  emit NDJSON on stdout (one object per note +
#                           one `tick_complete` summary).
#   --help                  show this help.
#
# Exit codes:
#   0 — success (including zero notes to promote)
#   1 — operator error (bad flags, no `.cosmon/` root, missing session)
#   2 — transient failure (nucleate failed) — launchd re-fires next tick.
#
# Invariants respected:
#   - §8b seal-as-trace: session files are NEVER mutated. Sidecar-only.
#   - §8j ingress: session is the human-carnet ingress port. Each spark
#     carries `source:session` + `stream:session-to-spark` tags so
#     downstream sees the provenance.
#   - ADR-016 no-daemon-in-core: one-shot script fired by external
#     scheduler (LaunchAgent) or operator (`cs session promote`).
#   - CLI-first: we invoke `cs nucleate spark` (via `cs spark` internals
#     through `nucleate`), not the MCP server.

set -euo pipefail

SELF_NAME="session-to-spark"
MAX_EVENTS=50
JSON_MODE=0
DRY_RUN=0
COSMON_ROOT=""
SESSION_FILTER=""
PROMOTE_NOTES=""
ALL_SPARK_PREFIX=0

usage() {
    sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "${SELF_NAME}: $*" >&2
    exit 1
}

emit_json() {
    local first=1
    printf '{'
    while (($#)); do
        local pair="$1"; shift
        local k="${pair%%=*}"
        local v="${pair#*=}"
        if (( first )); then first=0; else printf ','; fi
        local esc="${v//\\/\\\\}"
        esc="${esc//\"/\\\"}"
        esc="${esc//$'\n'/\\n}"
        printf '"%s":"%s"' "$k" "$esc"
    done
    printf '}\n'
}

# Walk up from $1 until a directory containing `.cosmon/state/sessions/` is found.
discover_root() {
    local dir="$1"
    dir="$(cd "$dir" && pwd -P)"
    while [[ "$dir" != "/" && -n "$dir" ]]; do
        if [[ -d "$dir/.cosmon/state/sessions" ]]; then
            printf '%s\n' "$dir"
            return 0
        fi
        dir="$(dirname "$dir")"
    done
    printf ''
}

# Extract a YAML scalar value from the opening frontmatter of a session
# file. Strips surrounding double-quotes. Returns empty string on miss.
# Stops at the first closing `---` at column zero, so the footer is not
# scanned.
yaml_get_header() {
    local key="$1" file="$2"
    awk -v k="$key" '
        BEGIN { in_fm = 0 }
        /^---[[:space:]]*$/ {
            if (in_fm == 0) { in_fm = 1; next }
            else { exit }
        }
        in_fm == 1 {
            n = index($0, ":")
            if (n == 0) next
            tag = substr($0, 1, n - 1)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", tag)
            if (tag != k) next
            val = substr($0, n + 1)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", val)
            if (length(val) >= 2 && substr(val, 1, 1) == "\"" && substr(val, length(val), 1) == "\"") {
                val = substr(val, 2, length(val) - 2)
            }
            print val
            exit
        }
    ' "$file"
}

# Parse CLI flags.
while (($#)); do
    case "$1" in
        --cosmon-root) COSMON_ROOT="$2"; shift 2 ;;
        --session) SESSION_FILTER="$2"; shift 2 ;;
        --promote-notes) PROMOTE_NOTES="$2"; shift 2 ;;
        --all-spark-prefix) ALL_SPARK_PREFIX=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --max) MAX_EVENTS="$2"; shift 2 ;;
        --json) JSON_MODE=1; shift ;;
        -h|--help|help) usage 0 ;;
        *) echo "${SELF_NAME}: unknown flag: $1" >&2; usage 1 ;;
    esac
done

# Default: if neither --all-spark-prefix nor --promote-notes, treat as
# --all-spark-prefix (the common case — operator runs the tick manually
# or via LaunchAgent expecting prefix-based promotion).
if (( ALL_SPARK_PREFIX == 0 )) && [[ -z "$PROMOTE_NOTES" ]]; then
    ALL_SPARK_PREFIX=1
fi

if [[ -z "$COSMON_ROOT" ]]; then
    COSMON_ROOT="$(discover_root "$(pwd)")"
    [[ -n "$COSMON_ROOT" ]] || die "no .cosmon/state/sessions/ found above $(pwd) — pass --cosmon-root <DIR>"
fi

SESSIONS_DIR="$COSMON_ROOT/.cosmon/state/sessions"
PROMOTED_DIR="$SESSIONS_DIR/.promoted"
[[ -d "$SESSIONS_DIR" ]] || die "$SESSIONS_DIR does not exist"
if (( DRY_RUN == 0 )); then
    mkdir -p "$PROMOTED_DIR"
fi

command -v cs >/dev/null 2>&1 || die "'cs' not on PATH"

# Normalise --session to an absolute path (if given). Accepts either a
# full path or a session_id like `session-2026-04-22T10-31-31Z`.
SESSION_PATH=""
if [[ -n "$SESSION_FILTER" ]]; then
    if [[ -f "$SESSION_FILTER" ]]; then
        SESSION_PATH="$(cd "$(dirname "$SESSION_FILTER")" && pwd -P)/$(basename "$SESSION_FILTER")"
    elif [[ -f "$SESSIONS_DIR/${SESSION_FILTER}.md" ]]; then
        SESSION_PATH="$SESSIONS_DIR/${SESSION_FILTER}.md"
    elif [[ -f "$SESSIONS_DIR/${SESSION_FILTER}" ]]; then
        SESSION_PATH="$SESSIONS_DIR/${SESSION_FILTER}"
    else
        die "session not found: $SESSION_FILTER"
    fi
fi

# Build the explicit-promote lookup map. Entries are stored as a
# newline-delimited list of `<session_id>@<ts>` pairs; lookup is a
# substring check surrounded by newlines to avoid prefix collisions.
# `@` is the separator because note timestamps are `HH:MM:SS` — colons
# make them ambiguous with a session_id:ts pair.
PROMOTE_MAP=""
if [[ -n "$PROMOTE_NOTES" ]]; then
    IFS=',' read -r -a promote_arr <<< "$PROMOTE_NOTES"
    for entry in "${promote_arr[@]}"; do
        entry="$(echo "$entry" | awk '{$1=$1};1')"  # trim
        [[ -z "$entry" ]] && continue
        # Shorthand: `<ts>` alone (no `@`) binds to the --session filter.
        if [[ "$entry" != *@* ]]; then
            if [[ -z "$SESSION_PATH" ]]; then
                die "--promote-notes entry '$entry' lacks a session_id; pass --session or use <session_id>@<ts>"
            fi
            sid="$(basename "$SESSION_PATH" .md)"
            entry="${sid}@${entry}"
        fi
        PROMOTE_MAP="${PROMOTE_MAP}
${entry}"
    done
fi

is_explicitly_promoted() {
    local session_id="$1" note_ts="$2"
    [[ -z "$PROMOTE_MAP" ]] && return 1
    local needle=$'\n'"${session_id}@${note_ts}"
    case "$PROMOTE_MAP" in
        *"$needle"*) return 0 ;;
        *) return 1 ;;
    esac
}

# Derive a nucleon_id: prefer the session frontmatter's `operator` field
# (the carnet's author), fall back to git config user.email, then to
# $USER@$(hostname). Keeps the §8j propagation clean.
derive_nucleon_id() {
    local session_file="$1"
    local op
    op="$(yaml_get_header operator "$session_file")"
    if [[ -n "$op" && "$op" != "unknown" ]]; then
        # If operator is a plain username, compose with hostname to
        # match the whisper convention. Otherwise assume it's already
        # fully-qualified.
        if [[ "$op" != *"@"* ]]; then
            local h
            h="$(hostname 2>/dev/null || echo unknown)"
            printf '%s@%s' "$op" "$h"
        else
            printf '%s' "$op"
        fi
        return 0
    fi
    local email
    email="$(git config --get user.email 2>/dev/null || true)"
    if [[ -n "$email" ]]; then
        printf '%s' "$email"
        return 0
    fi
    printf '%s@%s' "${USER:-unknown}" "$(hostname 2>/dev/null || echo unknown)"
}

found=0
sparked=0
skipped=0
failed=0

# Enumerate sessions to scan.
session_files=()
if [[ -n "$SESSION_PATH" ]]; then
    session_files+=("$SESSION_PATH")
else
    shopt -s nullglob
    for s in "$SESSIONS_DIR"/session-*.md; do
        session_files+=("$s")
    done
    shopt -u nullglob
fi

for session_file in "${session_files[@]}"; do
    [[ -f "$session_file" ]] || continue
    session_id="$(basename "$session_file" .md)"

    # Parse notes from the session body. Each note is a `## HH:MM:SS — tag`
    # heading followed by a body paragraph until the next heading or the
    # closing frontmatter. We stream the file through awk and emit one
    # record per note, separated by ASCII Unit Separator (0x1F) — a
    # non-whitespace byte that bash's `read` will not collapse even
    # between empty fields (tag is optional so we need faithful empty
    # fields).
    #
    # The body is reconstructed inside awk to keep the pipeline single-pass.
    # Newlines inside the body are encoded as `\n` literal two-char
    # sequences and decoded on read.
    note_records="$(awk -v US=$'\x1f' '
        BEGIN { fm = 0; past_fm = 0; have_note = 0 }
        {
            # Track frontmatter fences (opening + sealed footer).
            if ($0 ~ /^---[[:space:]]*$/) {
                if (fm == 0) { fm = 1; next }
                else if (past_fm == 0) { past_fm = 1; next }
                else {
                    # This is the opening of the sealed footer. Flush
                    # any pending note and stop processing the file.
                    if (have_note) { emit(); have_note = 0 }
                    exit
                }
            }
            if (past_fm != 1) next

            # Heading lines — `## HH:MM:SS — tag` or `## HH:MM:SS -- tag`.
            if ($0 ~ /^##[[:space:]]+[0-9][0-9]:[0-9][0-9]:[0-9][0-9]([[:space:]]+[—-].*)?$/) {
                if (have_note) { emit(); have_note = 0 }
                # Parse ts + tag.
                line = $0
                sub(/^##[[:space:]]+/, "", line)
                ts = substr(line, 1, 8)
                rest = substr(line, 9)
                gsub(/^[[:space:]]*[—-][[:space:]]*/, "", rest)
                gsub(/[[:space:]]+$/, "", rest)
                tag = rest
                body = ""
                have_note = 1
                next
            }

            if (have_note) {
                if (body == "") body = $0
                else body = body "\\n" $0
            }
        }
        END { if (have_note) emit() }
        function emit() {
            # Trim leading/trailing blank separators.
            gsub(/^(\\n)+/, "", body)
            gsub(/(\\n)+$/, "", body)
            if (body == "") return
            printf "%s%s%s%s%s\n", ts, US, tag, US, body
        }
    ' "$session_file")"

    # Iterate notes.
    while IFS=$'\x1f' read -r note_ts note_tag note_body_raw; do
        [[ -z "$note_ts" ]] && continue
        (( found++ )) || true

        # Decode the body (undo the awk `\n` encoding).
        note_body="${note_body_raw//\\n/$'\n'}"

        # Strip `!spark ` prefix if present, and flag.
        has_prefix=0
        promoted_body="$note_body"
        if [[ "${note_body:0:7}" == "!spark " ]]; then
            has_prefix=1
            promoted_body="${note_body:7}"
        elif [[ "$note_body" == "!spark" ]]; then
            # Body is literally `!spark` with no text — reject.
            (( failed++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=note_rejected" \
                    "reason=empty_spark_body" \
                    "session=$session_id" \
                    "note_ts=$note_ts"
            else
                echo "${SELF_NAME}: reject (empty !spark body): $session_id@$note_ts" >&2
            fi
            continue
        fi

        explicit=0
        if is_explicitly_promoted "$session_id" "$note_ts"; then
            explicit=1
        fi

        # Selection rule: promote if (prefix mode AND has_prefix) OR (explicit).
        select_note=0
        if (( explicit )); then
            select_note=1
        elif (( ALL_SPARK_PREFIX )) && (( has_prefix )); then
            select_note=1
        fi

        if (( select_note == 0 )); then
            if (( JSON_MODE )); then
                emit_json \
                    "event=note_ignored" \
                    "reason=not_selected" \
                    "session=$session_id" \
                    "note_ts=$note_ts"
            fi
            continue
        fi

        # Idempotence — sidecar presence means we already promoted.
        sidecar_dir="$PROMOTED_DIR/$session_id"
        # Replace colons in the timestamp for a filesystem-safe sidecar name.
        ts_safe="${note_ts//:/-}"
        sidecar="$sidecar_dir/${ts_safe}.md"
        if [[ -e "$sidecar" ]]; then
            (( skipped++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=note_skipped" \
                    "reason=already_promoted" \
                    "session=$session_id" \
                    "note_ts=$note_ts"
            else
                echo "${SELF_NAME}: skip (already promoted): $session_id@$note_ts"
            fi
            continue
        fi

        # Reject empty body post-strip.
        if [[ -z "${promoted_body//[[:space:]]/}" ]]; then
            (( failed++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=note_rejected" \
                    "reason=empty_body" \
                    "session=$session_id" \
                    "note_ts=$note_ts"
            else
                echo "${SELF_NAME}: reject (empty body): $session_id@$note_ts" >&2
            fi
            continue
        fi

        if (( sparked >= MAX_EVENTS )); then
            break 2
        fi

        nucleon_id="$(derive_nucleon_id "$session_file")"

        if (( DRY_RUN )); then
            if (( JSON_MODE )); then
                emit_json \
                    "event=note_would_promote" \
                    "session=$session_id" \
                    "note_ts=$note_ts" \
                    "nucleon_id=$nucleon_id" \
                    "body_excerpt=${promoted_body:0:80}"
            else
                echo "${SELF_NAME}: would promote $session_id@$note_ts — ${promoted_body:0:80}"
            fi
            (( sparked++ )) || true
            continue
        fi

        # Build tags — same shape as whisper-to-spark for downstream parity.
        ts_tag="${note_ts//:/-}"
        args=(
            nucleate spark
            --var "topic=$promoted_body"
            --var "nucleon_id=$nucleon_id"
            --var "session_source=$session_file"
            --var "session_id=$session_id"
            --var "note_timestamp=$note_ts"
            --tag temp:hot
            --tag source:session
            --tag stream:session-to-spark
            --tag "session-note:${session_id}@${ts_tag}"
            --no-parent
        )
        if [[ -n "$note_tag" ]]; then
            args+=(--var "note_tag=$note_tag")
        fi

        if ! out="$(cd "$COSMON_ROOT" && cs --json "${args[@]}" 2>&1)"; then
            (( failed++ )) || true
            if (( JSON_MODE )); then
                emit_json \
                    "event=note_failed" \
                    "reason=nucleate_error" \
                    "session=$session_id" \
                    "note_ts=$note_ts" \
                    "stderr=$out"
            else
                echo "${SELF_NAME}: nucleate failed for $session_id@$note_ts: $out" >&2
            fi
            continue
        fi

        spark_id="$(printf '%s' "$out" \
            | grep -oE '"id"[[:space:]]*:[[:space:]]*"[^"]+"' \
            | head -1 \
            | sed -E 's/^.*:[[:space:]]*"([^"]+)".*$/\1/')"

        # Derive a single source-mode label for the sidecar frontmatter.
        # "explicit" wins when the operator named this note by hand,
        # even if the body also happens to carry the `!spark ` prefix.
        if (( explicit )); then
            source_mode="explicit"
        elif (( has_prefix )); then
            source_mode="prefix"
        else
            source_mode="unknown"
        fi

        # Write sidecar — idempotence marker + audit trail.
        mkdir -p "$sidecar_dir"
        {
            printf '%s\n' "---"
            printf 'session_id: %s\n' "$session_id"
            printf 'note_timestamp: %s\n' "$note_ts"
            printf 'promoted_at: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
            printf 'spark_id: %s\n' "$spark_id"
            printf 'nucleon_id: %s\n' "$nucleon_id"
            printf 'source: %s\n' "$source_mode"
            printf '%s\n' "---"
            printf '\n%s\n' "$promoted_body"
        } > "$sidecar"

        (( sparked++ )) || true
        if (( JSON_MODE )); then
            emit_json \
                "event=spark_created" \
                "spark_id=$spark_id" \
                "session=$session_id" \
                "note_ts=$note_ts" \
                "nucleon_id=$nucleon_id" \
                "sidecar=$sidecar"
        else
            echo "${SELF_NAME}: sparked $spark_id from $session_id@$note_ts (nucleon=$nucleon_id)"
        fi
    done <<< "$note_records"
done

if (( JSON_MODE )); then
    emit_json \
        "event=tick_complete" \
        "found=$found" \
        "sparked=$sparked" \
        "skipped=$skipped" \
        "failed=$failed" \
        "dry_run=$DRY_RUN"
else
    echo "${SELF_NAME}: tick_complete — found=$found sparked=$sparked skipped=$skipped failed=$failed${DRY_RUN:+ (dry-run)}"
fi

if (( failed > 0 )); then
    exit 2
fi
exit 0
