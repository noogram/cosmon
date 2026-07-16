#!/usr/bin/env bash
# cs-drop-ssh-wrapper.sh — scope-limited SSH endpoint for iPhone Shortcut drops.
#
# Install on the Mac; reference from `~/.ssh/authorized_keys` with
# `command=` restriction so the Shortcut's SSH key can ONLY invoke
# `cs drop` (or `cs spark` as fallback) — nothing else.
#
# Usage (from ~/.ssh/authorized_keys):
#   command="/srv/cosmon/cosmon/scripts/cs-drop-ssh-wrapper.sh",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty ssh-ed25519 AAAA... iphone-shortcut
#
# Input contract: the Shortcut writes the spark text to SSH_ORIGINAL_COMMAND
# (passed by sshd via the `command=` forced-command mechanism), or — if
# SSH_ORIGINAL_COMMAND is empty — reads the text from stdin.
#
# The first line of SSH_ORIGINAL_COMMAND may carry a `--galaxy <name>` flag;
# anything after that is the spark text. Example:
#
#   ssh mac "--galaxy mailroom  reread the pitch deck"
#
# Lands a spark in the `mailroom` galaxy with topic
# "reread the pitch deck". No flag → lands in the DEFAULT_GALAXY set below.
#
# Output: one line on stdout — the newly-nucleated molecule id (e.g.
# `spark-20260424-abcd`). The Shortcut displays this to the operator.
# Non-zero exit = failure; stderr carries the reason.

set -euo pipefail

# ----- Configuration ---------------------------------------------------------
# Edit these two values on the Mac where this script lives.

# Where your cosmon galaxies live. The wrapper will `cd` into the chosen
# galaxy so `cs` walk-up discovery lands on the right `.cosmon/` root.
GALAXIES_ROOT="${COSMON_GALAXIES_ROOT:-$HOME/galaxies}"

# Default galaxy when the Shortcut does not pass `--galaxy`.
DEFAULT_GALAXY="${COSMON_DEFAULT_GALAXY:-cosmon}"

# Absolute path to the `cs` binary. `~/.zshrc` does NOT run in non-interactive
# SSH sessions, so we cannot rely on PATH — state the path explicitly.
CS_BIN="${CS_BIN:-$HOME/.cargo/bin/cs}"

# ----- Parse ------------------------------------------------------------------
raw="${SSH_ORIGINAL_COMMAND:-}"
if [[ -z "$raw" ]]; then
  # No forced-command payload; fall back to stdin (manual testing).
  raw="$(cat)"
fi

galaxy="$DEFAULT_GALAXY"
text="$raw"
if [[ "$raw" == --galaxy\ * ]]; then
  # `--galaxy <name> <text...>`
  rest="${raw#--galaxy }"
  galaxy="${rest%% *}"
  text="${rest#* }"
fi

# Trim leading/trailing whitespace.
text="$(printf '%s' "$text" | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"

if [[ -z "$text" ]]; then
  echo "cs-drop-ssh-wrapper: empty spark text" >&2
  exit 2
fi

# ----- Dispatch ---------------------------------------------------------------
galaxy_dir="$GALAXIES_ROOT/$galaxy"
if [[ ! -d "$galaxy_dir/.cosmon" ]]; then
  echo "cs-drop-ssh-wrapper: no .cosmon at $galaxy_dir" >&2
  exit 3
fi

cd "$galaxy_dir"

# Prefer `cs drop` when it lands (C-DROP-GESTURE). Fall back to `cs spark`
# so this wrapper already works on today's binary.
if "$CS_BIN" drop --help >/dev/null 2>&1; then
  verb=(drop)
else
  verb=(spark)
fi

# `--json` so the Shortcut can extract the molecule id reliably.
# `--tag source:shortcut` tags every iPhone-origin spark for later triage
# (`cs ensemble --tag source:shortcut`).
out="$("$CS_BIN" --json "${verb[@]}" \
  --tag source:shortcut \
  --tag stream:mobile \
  "$text")"

# Extract the molecule id. `cs --json nucleate/spark/drop` emits a single
# JSON object with an `"id":"<prefix>-YYYYMMDD-xxxx"` field.
mol_id="$(printf '%s\n' "$out" \
  | sed -n 's/.*"id":"\([a-z]*-[0-9a-z-]*\)".*/\1/p' \
  | head -n 1)"

if [[ -z "$mol_id" ]]; then
  echo "cs-drop-ssh-wrapper: no molecule_id in output" >&2
  printf '%s\n' "$out" >&2
  exit 4
fi

# Single-line stdout the Shortcut can pipe into its `Show Result` step.
echo "$mol_id"
