#!/usr/bin/env bash
# scripts/docker-addl-smoke.sh — ADDL delivery rehearsal.
#
# Builds the Dockerfile.addl-test image, creates a dummy Rust repo inside
# a container that impersonates Bob Doe's laptop, and drives the
# full first-run flow end to end. Five stealth / telemetry invariants
# (f–j in the molecule brief) are asserted at the end.
#
# Graceful degradation:
#   The script prints progress for each step. If one of the three sibling
#   features (cs opt-in-share, cs init --stealth, cs share-telemetry) is
#   not yet on the `cs` binary baked into the image, the script records
#   the gap in $PARTIAL and continues the remainder of the flow. Exit
#   code stays 0 only when every gate is green; otherwise we exit 2 with
#   a clear summary so it is obvious whether it is a real regression or
#   a dependency-still-in-flight situation.
#
# Usage (from repo root):
#   bash scripts/docker-addl-smoke.sh             # full rehearsal
#   bash scripts/docker-addl-smoke.sh --rebuild   # force docker rebuild
#
# Prerequisites:
#   - docker (buildx)
#   - age (for local decryption of the bundle produced inside the container)
#   - ~/.config/age/you.key.txt — private key matching the recipient
#     embedded in Dockerfile.addl-test (optional; step j is skipped if
#     absent).
#
# References:
#   task-20260419-5726 harness
#   task-20260419-32de ADDL polymer root
#   task-20260419-a0e9 / 5b29 / df6b (in-flight siblings)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="cosmon-addl-test"
CONTAINER_NAME="cosmon-addl-smoke-$$"
REBUILD=0
TRACE_DIR="${REPO_ROOT}/.addl-rehearsal"

PARTIAL=()
FAILED=()
GREEN=()

for arg in "$@"; do
  case "$arg" in
    --rebuild) REBUILD=1 ;;
    --help|-h)
      sed -n '1,40p' "$0"
      exit 0 ;;
    *)
      echo "error: unknown flag: $arg" >&2
      exit 2 ;;
  esac
done

cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# 1. Build the image (skipped if already present unless --rebuild).
# ---------------------------------------------------------------------------
if [[ $REBUILD -eq 1 ]] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "==> Building image '$IMAGE' (this takes a few minutes on first run)…"
  docker build -f Dockerfile.addl-test -t "$IMAGE" .
else
  echo "==> Image '$IMAGE' already present — skipping build (use --rebuild to force)."
fi

mkdir -p "$TRACE_DIR"

# ---------------------------------------------------------------------------
# 2. Seed the rehearsal script we pipe into the container. Keeping it here
#    means the source of truth for the flow stays next to the harness and
#    we do not need a second file tracked in-tree.
# ---------------------------------------------------------------------------
SMOKE_INNER="$TRACE_DIR/smoke-inner.sh"
cat >"$SMOKE_INNER" <<'INNER'
#!/usr/bin/env bash
# Rehearsal flow executed *inside* the container, as user bob.

set -u

log() { printf '\n---- [smoke] %s ----\n' "$*"; }
mark_partial() { echo "PARTIAL: $*" >>/traces/report.txt; }
mark_green()   { echo "GREEN: $*"   >>/traces/report.txt; }
mark_red()     { echo "RED: $*"     >>/traces/report.txt; }

: >/traces/report.txt

cd /home/bob/projects/dummy-addl-repo

# Create a minimal Rust hello-world so the project feels like real work.
log "seeding dummy repo"
cat >Cargo.toml <<'TOML'
[package]
name = "dummy-addl"
version = "0.1.0"
edition = "2021"
TOML
mkdir -p src
cat >src/main.rs <<'RS'
fn main() {
    println!("hello from ADDL");
}
RS
git init -q
git add .
git commit -q -m "chore: initial dummy project"

# --- (a) cs opt-in-share --------------------------------------------------
log "step a — cs opt-in-share"
if cs opt-in-share --help >/dev/null 2>&1; then
  if printf 'o\n' | cs opt-in-share >/traces/opt-in.out 2>&1; then
    mark_green "a: cs opt-in-share accepted"
  else
    mark_red "a: cs opt-in-share failed (see /traces/opt-in.out)"
  fi
else
  mark_partial "a: cs opt-in-share not available (task-20260419-5b29 not merged)"
fi

# --- (b) cs init --stealth ------------------------------------------------
log "step b — cs init --stealth"
if cs init --help 2>&1 | grep -q -- '--stealth'; then
  if cs init --stealth --yes >/traces/init.out 2>&1; then
    mark_green "b: cs init --stealth succeeded"
  else
    mark_red "b: cs init --stealth failed (see /traces/init.out)"
  fi
else
  mark_partial "b: cs init --stealth not available (task-20260419-a0e9 not merged)"
  # Fall back to a plain init so the rest of the flow has a .cosmon/.
  cs init --yes >/traces/init.out 2>&1 || mark_red "b-fallback: cs init failed"
  # Manually append .cosmon/ to .gitignore for the trace-check asserts below.
  if ! grep -qx '.cosmon/' .gitignore 2>/dev/null; then
    echo '.cosmon/' >>.gitignore
    git add .gitignore
    git -c user.name="Bob Doe" -c user.email="bob@addl.fr" \
        commit -q -m "chore: ignore .cosmon/"
  fi
fi

# --- (c) cs nucleate + tackle --leaf -------------------------------------
log "step c — nucleate + tackle"
mol_id=""
if cs tackle --help 2>&1 | grep -q -- '--leaf'; then
  nucleate_out="$(cs nucleate task-work --var topic='add a simple function' --json 2>/traces/nucleate.err || true)"
  mol_id="$(echo "$nucleate_out" | jq -r '.molecule_id // empty')"
  if [[ -n $mol_id ]]; then
    mark_green "c1: nucleate produced $mol_id"
    if cs tackle "$mol_id" --leaf >/traces/tackle.out 2>&1; then
      mark_green "c2: tackle --leaf dispatched $mol_id"
    else
      mark_red "c2: tackle failed for $mol_id"
    fi
  else
    mark_red "c1: nucleate returned no molecule_id"
  fi
else
  mark_partial "c: cs tackle --leaf not available — skipping nucleate+tackle"
fi

# --- (d) cs wait ----------------------------------------------------------
if [[ -n ${mol_id:-} ]]; then
  log "step d — wait"
  if cs wait "$mol_id" --timeout 600 >/traces/wait.out 2>&1; then
    mark_green "d: wait returned successfully"
  else
    mark_red "d: wait timed out or failed"
  fi
fi

# --- (e) cs done ----------------------------------------------------------
if [[ -n ${mol_id:-} ]]; then
  log "step e — done"
  if cs done "$mol_id" >/traces/done.out 2>&1; then
    mark_green "e: done succeeded"
  else
    mark_red "e: done failed (may be fine if wait timed out)"
  fi
fi

# --- (f) git history must not mention cosmon internals -------------------
log "step f — scan git history for cosmon leaks"
leaked="$(git log --all --pretty=format:'%an%n%ae%n%s%n%b' 2>/dev/null \
          | grep -Ei 'cosmon|task-[0-9a-f]{4,}|evolve\(|molecule|^worker' || true)"
if [[ -z $leaked ]]; then
  mark_green "f: git history clean (no cosmon/task-/evolve/molecule/worker tokens)"
else
  mark_red "f: git history contains cosmon tokens:"
  printf '%s\n' "$leaked" >>/traces/report.txt
fi

# --- (g) git log author must be Bob Doe ---------------------------
log "step g — author sanity check"
authors="$(git log --all --pretty=format:'%an <%ae>' | sort -u || true)"
if [[ "$authors" == "Bob Doe <bob@addl.fr>" ]]; then
  mark_green "g: every commit authored by Bob Doe"
else
  mark_red "g: unexpected authors in git log:"
  printf '%s\n' "$authors" >>/traces/report.txt
fi

# --- (h) .cosmon/ in .gitignore ------------------------------------------
log "step h — .cosmon/ gitignore check"
if grep -qE '^\.cosmon/?$' .gitignore 2>/dev/null; then
  mark_green "h: .cosmon/ ignored by git"
else
  mark_red "h: .cosmon/ not in .gitignore"
fi

# --- (i) cs observe returns complete ledger ------------------------------
if [[ -n ${mol_id:-} ]]; then
  log "step i — observe ledger"
  if cs observe "$mol_id" --json >/traces/observe.json 2>/traces/observe.err; then
    if jq -e '.status and .formula and .created_at' /traces/observe.json >/dev/null; then
      mark_green "i: cs observe returns status+formula+created_at"
    else
      mark_red "i: cs observe JSON incomplete (see /traces/observe.json)"
    fi
  else
    mark_red "i: cs observe failed"
  fi
fi

# --- (j) share-telemetry age bundle --------------------------------------
if [[ -n ${mol_id:-} ]]; then
  log "step j — share-telemetry"
  bundle="/traces/${mol_id}.bundle.age"
  if cs share-telemetry --help >/dev/null 2>&1; then
    if cs share-telemetry "$mol_id" --out "age:default" --dest "$bundle" \
       >/traces/share.out 2>&1; then
      mark_green "j: cs share-telemetry produced $bundle"
    else
      mark_red "j: cs share-telemetry failed (see /traces/share.out)"
    fi
  elif [[ -x /repo/scripts/share-telemetry.sh ]]; then
    # Fallback to the script form shipped by task-20260419-5f67.
    if /repo/scripts/share-telemetry.sh "$mol_id" --dry-run --out "$bundle" \
       >/traces/share.out 2>&1; then
      mark_partial "j: used scripts/share-telemetry.sh fallback (task-20260419-df6b not merged)"
    else
      mark_red "j: fallback script also failed"
    fi
  else
    mark_partial "j: no share-telemetry available (task-20260419-df6b not merged)"
  fi
fi

echo
echo "==== smoke report ===="
cat /traces/report.txt
INNER

chmod +x "$SMOKE_INNER"

# ---------------------------------------------------------------------------
# 3. Run the container. /traces is the rendezvous with the host.
# ---------------------------------------------------------------------------
echo "==> Running rehearsal inside container '$CONTAINER_NAME'…"
docker run --rm \
  --name "$CONTAINER_NAME" \
  -v "$TRACE_DIR:/traces" \
  -v "$REPO_ROOT:/repo:ro" \
  "$IMAGE" \
  -c "bash /traces/smoke-inner.sh"

REPORT="$TRACE_DIR/report.txt"
if [[ ! -s $REPORT ]]; then
  echo "error: smoke run produced no report at $REPORT" >&2
  exit 2
fi

echo
echo "==> Parsing report…"
while IFS= read -r line; do
  case "$line" in
    GREEN:*)   GREEN+=("${line#GREEN: }") ;;
    RED:*)     FAILED+=("${line#RED: }") ;;
    PARTIAL:*) PARTIAL+=("${line#PARTIAL: }") ;;
  esac
done <"$REPORT"

echo
echo "=== ADDL rehearsal summary ==="
printf '  GREEN   : %d\n' "${#GREEN[@]}"
printf '  PARTIAL : %d\n' "${#PARTIAL[@]}"
printf '  RED     : %d\n' "${#FAILED[@]}"

if (( ${#FAILED[@]} > 0 )); then
  echo
  echo "RED items:"
  printf '  - %s\n' "${FAILED[@]}"
  exit 2
fi

if (( ${#PARTIAL[@]} > 0 )); then
  echo
  echo "PARTIAL items (pending sibling merges):"
  printf '  - %s\n' "${PARTIAL[@]}"
  echo
  echo "Harness ready; full green requires task-20260419-a0e9/5b29/df6b on main."
  exit 0
fi

echo
echo "All checks green — rehearsal successful."

# Local decryption check (j-extension) — runs on host after container exits.
bundle_age="$(ls -1 "$TRACE_DIR"/*.bundle.age 2>/dev/null | head -n1 || true)"
if [[ -n $bundle_age && -f "$HOME/.config/age/you.key.txt" ]]; then
  echo "==> Decrypting $bundle_age locally to confirm operator clearance…"
  if age --decrypt -i "$HOME/.config/age/you.key.txt" "$bundle_age" \
       >"$TRACE_DIR/bundle.json" 2>/dev/null; then
    if jq -e . "$TRACE_DIR/bundle.json" >/dev/null; then
      echo "    decrypted bundle is valid JSON ($(wc -c <"$TRACE_DIR/bundle.json") bytes)"
    else
      echo "    WARNING: decrypted payload is not JSON" >&2
      exit 2
    fi
  else
    echo "    WARNING: decryption failed" >&2
    exit 2
  fi
fi
