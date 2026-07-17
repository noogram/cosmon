#!/usr/bin/env bash
# quickstart-wikipedia.sh — one-shot bootstrap for a wikipedia-production project.
#
# Usage:
#   scripts/quickstart-wikipedia.sh <project-dir> <subject> <pdf>...
#
# Composes existing `cs` verbs only. No new Rust.
set -euo pipefail

usage() {
    cat >&2 <<'USAGE'
Usage: quickstart-wikipedia.sh <project-dir> <subject> <pdf>...
  <project-dir>  Directory to create (must not exist).
  <subject>      Wikipedia-style noun-phrase title (e.g. "Kuramoto model").
  <pdf>...       One or more readable PDF paths to stage as sources/.
USAGE
    exit 2
}

[ "$#" -ge 3 ] || usage

PROJECT_DIR="$1"; shift
SUBJECT="$1"; shift
PDFS=("$@")

# Resolve COSMON_ROOT = parent of the directory containing this script.
SCRIPT_PATH="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
COSMON_ROOT="$(cd "$(dirname "$SCRIPT_PATH")/.." && pwd)"
TEMPLATE_DIR="$COSMON_ROOT/templates/wikipedia-production"

# ── Preflight ──────────────────────────────────────────────────────
# Each missing prereq prints an actionable install hint so a fresh
# macOS/Linux user can recover without reading the script.
missing=0
need() {
    local cmd="$1" hint="$2"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "error: '$cmd' not on PATH — $hint" >&2
        missing=1
    fi
}
need cs    "install with: cargo install --path crates/cosmon-cli --locked"
need git   "install via your package manager (brew install git / apt-get install git)"
need tmux  "install via your package manager (brew install tmux / apt-get install tmux)"
need cargo "install the Rust toolchain: https://rustup.rs"
[ "$missing" -eq 0 ] || exit 1

# ── Model-backend probe (advisory, non-fatal) ──────────────────────
# The mission dispatches a REAL worker. With the default (local) adapter
# that worker drives an OpenAI-compatible endpoint — Ollama on
# localhost:11434 out of the box. If nothing is listening the worker has
# nothing to talk to and stalls SILENTLY at `cs tackle` below. We cannot
# hard-fail here (an external adapter or a non-default endpoint may be
# configured), so we warn loudly rather than let the stall be the first
# symptom the operator sees.
backend_answers() {
    local url="$1"
    if command -v curl >/dev/null 2>&1; then
        # Any HTTP answer (even 404) means something is listening; a
        # refused connection or timeout is a non-zero exit.
        curl -sS --max-time 2 -o /dev/null "$url" >/dev/null 2>&1
    else
        # Fallback: raw TCP connect test on host:port via bash /dev/tcp.
        local hostport host port
        hostport="${url#*://}"; hostport="${hostport%%/*}"
        host="${hostport%%:*}"; port="${hostport##*:}"
        [ "$port" = "$hostport" ] && port=11434
        (exec 3<>"/dev/tcp/${host}/${port}") >/dev/null 2>&1 && exec 3>&- 3<&-
    fi
}

if [ -z "${COSMON_DEFAULT_ADAPTER:-}" ]; then
    BACKEND_URL="${COSMON_LOCAL_ENDPOINT:-http://localhost:11434}"
    if ! backend_answers "$BACKEND_URL"; then
        echo "warning: no model backend answering at ${BACKEND_URL}" >&2
        echo "  the default adapter drives a local model there — start it before dispatch:" >&2
        echo "    ollama serve" >&2
        echo "  (or configure an external adapter via COSMON_DEFAULT_ADAPTER / .cosmon/config.toml)." >&2
        echo "  without a reachable backend the worker stalls silently at 'cs tackle'." >&2
    fi
fi

# Rust ≥ 1.88 (workspace MSRV — must track Cargo.toml `rust-version`).
if command -v rustc >/dev/null 2>&1; then
    rustc_ver="$(rustc --version | awk '{print $2}')"
    rustc_major="$(printf '%s' "$rustc_ver" | cut -d. -f1)"
    rustc_minor="$(printf '%s' "$rustc_ver" | cut -d. -f2)"
    if [ "$rustc_major" -lt 1 ] || { [ "$rustc_major" -eq 1 ] && [ "$rustc_minor" -lt 88 ]; }; then
        echo "error: rustc ${rustc_ver} is below MSRV 1.88 — run: rustup update stable" >&2
        exit 1
    fi
fi

# Git identity — `git commit` below fails silently on a fresh machine
# without these set.
if ! git config --get user.email >/dev/null 2>&1 || ! git config --get user.name >/dev/null 2>&1; then
    echo "error: git user.name / user.email not configured." >&2
    echo "  git config --global user.name  'Your Name'" >&2
    echo "  git config --global user.email 'you@example.com'" >&2
    exit 1
fi

[ -d "$TEMPLATE_DIR" ] || { echo "error: template not found: $TEMPLATE_DIR" >&2; exit 1; }

if [ -e "$PROJECT_DIR" ]; then
    echo "error: project directory already exists: $PROJECT_DIR" >&2
    exit 1
fi

for pdf in "${PDFS[@]}"; do
    [ -r "$pdf" ] || { echo "error: PDF not readable: $pdf" >&2; exit 1; }
done

# Absolute PDF paths (resolve before cd).
ABS_PDFS=()
for pdf in "${PDFS[@]}"; do
    ABS_PDFS+=("$(cd "$(dirname "$pdf")" && pwd)/$(basename "$pdf")")
done

# ── Bootstrap ──────────────────────────────────────────────────────
mkdir -p "$PROJECT_DIR"
cd "$PROJECT_DIR"

# cs leaves git alone (its own onboarding says "Initialize git yourself"),
# and the commit at the end of this script needs a repository — own it here.
git rev-parse --git-dir >/dev/null 2>&1 || git init -q

cs init --yes

# Copy template contents (dotfiles included).
cp -R "$TEMPLATE_DIR"/. .

# Stage PDFs into sources/.
mkdir -p sources
for pdf in "${ABS_PDFS[@]}"; do
    cp "$pdf" sources/
done

# Render MISSION.md from heredoc. Build a TOML array of source filenames.
SOURCES_LIST=""
for pdf in "${ABS_PDFS[@]}"; do
    base="$(basename "$pdf")"
    if [ -z "$SOURCES_LIST" ]; then
        SOURCES_LIST="\"$base\""
    else
        SOURCES_LIST="$SOURCES_LIST, \"$base\""
    fi
done

cat > MISSION.md <<EOF
+++
subject = "${SUBJECT}"
scope = "To be refined by editor-in-chief from the staged sources."

[sources]
policy = "secondary-preferred"
files = [${SOURCES_LIST}]

quality = "C"
adversarial_intensity = "light"
+++

# ${SUBJECT}

<!--
  Free-form editorial notes. The editor-in-chief reads this file to
  derive scope.md and the assessment-matrix.md. Staged PDFs live in
  ./sources/.
-->
EOF

# Remove the template placeholder so it does not shadow MISSION.md.
rm -f MISSION.md.tmpl

# ── Initial commit (cs tackle needs at least one commit to branch from) ──
git add -A
git commit -m "feat: bootstrap ${SUBJECT} via quickstart"

# ── Dispatch ───────────────────────────────────────────────────────
MOL_JSON="$(cs nucleate mission-controller --json)"
MOL_ID="$(printf '%s' "$MOL_JSON" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"

if [ -z "$MOL_ID" ]; then
    echo "error: could not parse molecule id from: $MOL_JSON" >&2
    exit 1
fi

cs tackle "$MOL_ID"

# ── Smoke test ─────────────────────────────────────────────────────
# Prove the install actually produced a live, observable molecule.
# Fails loud if the state dir or the observe read-back is missing.
if [ ! -d ".cosmon/state/molecules/$MOL_ID" ] \
   && [ ! -d ".cosmon/state/fleets/default/molecules/$MOL_ID" ]; then
    echo "error: smoke-test failed — molecule state dir missing for $MOL_ID" >&2
    exit 1
fi
if ! cs observe "$MOL_ID" --json >/dev/null 2>&1; then
    echo "error: smoke-test failed — 'cs observe $MOL_ID --json' did not succeed" >&2
    exit 1
fi
echo "✓ smoke-test: molecule $MOL_ID is observable"

cat <<EOF

✓ Wikipedia-production project bootstrapped at: $PROJECT_DIR
  Subject : $SUBJECT
  Sources : ${#ABS_PDFS[@]} PDF(s) staged in sources/
  Mission : $MOL_ID (tackled)

Next:
  cs wait $MOL_ID & cs peek
  cs done $MOL_ID
EOF
