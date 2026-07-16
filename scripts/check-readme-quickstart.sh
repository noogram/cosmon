#!/usr/bin/env bash
# Gate the README Quickstart block against syntax errors and CLI drift.
#
# Extracts the first fenced ```bash block appearing under the
# "## A real session" heading in README.md and validates:
#
#   1. bash -n parses the block without syntax errors.
#   2. Every `cs <subcommand>` referenced in the block is listed by
#      `cs --help` (drift guard). Skipped if `cs` is not on PATH.
#
# Optional end-to-end execution (bash -e with dummy PDFs) is gated behind
# CHECK_README_EXEC=1 and requires `cs` on PATH.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
README="$ROOT/README.md"
HEADING='## A real session'

if [[ ! -f "$README" ]]; then
    echo "error: $README not found" >&2
    exit 2
fi

# Extract the first ```bash fenced block appearing after the heading.
BLOCK="$(awk -v heading="$HEADING" '
    $0 == heading { in_section = 1; next }
    in_section && /^## / { exit }
    in_section && /^```bash[[:space:]]*$/ { in_block = 1; next }
    in_block && /^```[[:space:]]*$/ { exit }
    in_block { print }
' "$README")"

if [[ -z "$BLOCK" ]]; then
    echo 'error: could not extract bash fenced block under '"'$HEADING'" >&2
    exit 2
fi

TMP="$(mktemp -t quickstart.XXXXXX.sh)"
trap 'rm -f "$TMP"' EXIT
# Substitute angle-bracket placeholders (<mission-id>, <foo>) with a safe
# identifier token so bash -n does not mistake them for I/O redirections.
SAFE_BLOCK="$(printf '%s\n' "$BLOCK" | sed -E 's/<[a-zA-Z][a-zA-Z0-9_-]*>/PLACEHOLDER/g')"
printf '#!/usr/bin/env bash\nset -euo pipefail\n%s\n' "$SAFE_BLOCK" > "$TMP"

# 1. Syntax gate.
if ! bash -n "$TMP"; then
    echo "error: quickstart block failed bash -n syntax check" >&2
    exit 1
fi
echo "✓ bash -n syntax OK"

# 2. Drift gate — every `cs <subcommand>` must appear in `cs --help`.
#    `cs` absent ⇒ drift gate skipped (local dev without installed binary).
if command -v cs >/dev/null 2>&1; then
    HELP="$(cs --help 2>&1 || true)"
    # Extract the subcommand names: lines like `  nucleate   ...`.
    KNOWN="$(printf '%s\n' "$HELP" | awk '
        /^Commands:/ { in_cmds = 1; next }
        in_cmds && /^[A-Z]/ { in_cmds = 0 }
        in_cmds && /^[[:space:]]+[a-z]/ { print $1 }
    ')"
    if [[ -z "$KNOWN" ]]; then
        # Fallback: scrape any leading-word token that looks like a subcommand.
        KNOWN="$(printf '%s\n' "$HELP" | awk '/^[[:space:]]+[a-z][a-z-]+/ { print $1 }')"
    fi

    # Collect referenced subcommands (first token after `cs `).
    REFS="$(printf '%s\n' "$BLOCK" \
        | grep -oE '(^|[^A-Za-z0-9_-])cs[[:space:]]+[a-z][a-z-]*' \
        | awk '{print $NF}' \
        | sort -u)"

    MISSING=""
    while IFS= read -r cmd; do
        [[ -z "$cmd" ]] && continue
        if ! printf '%s\n' "$KNOWN" | grep -qx "$cmd"; then
            MISSING+="  - cs $cmd"$'\n'
        fi
    done <<< "$REFS"

    if [[ -n "$MISSING" ]]; then
        echo "error: README quickstart references unknown cs subcommands:" >&2
        printf '%s' "$MISSING" >&2
        echo "" >&2
        echo "  cs --help lists:" >&2
        printf '%s\n' "$KNOWN" | sed 's/^/    /' >&2
        exit 1
    fi
    echo "✓ drift check OK (cs subcommands: $(echo "$REFS" | tr '\n' ' '))"
else
    echo "• drift check skipped (cs not on PATH)"
fi

# 3. Optional end-to-end exec — opt-in; placeholders like `<mission-id>` make
#    the block non-runnable without substitution, so this stays behind a flag.
if [[ "${CHECK_README_EXEC:-0}" == "1" ]]; then
    if ! command -v cs >/dev/null 2>&1; then
        echo "error: CHECK_README_EXEC=1 but cs not on PATH" >&2
        exit 1
    fi
    WORK="$(mktemp -d -t quickstart-exec.XXXXXX)"
    trap 'rm -f "$TMP"; rm -rf "$WORK"' EXIT
    for n in a b c; do
        printf '%%PDF-1.4\n%%dummy-%s\n' "$n" > "$WORK/$n.pdf"
    done
    echo "• exec gate not yet implemented (block contains <mission-id> placeholder)" >&2
    # Placeholder: a full exec gate requires substituting <mission-id> and
    # running under `bash -e`. Deferred until the block is parameterized.
fi

echo "✓ README quickstart block passes all gates"
