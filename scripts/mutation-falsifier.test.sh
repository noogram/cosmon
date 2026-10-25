#!/usr/bin/env bash
# mutation-falsifier.test.sh — regression tests for the mutation-falsifier gate
# (scripts/mutation-falsifier.sh).
#
# Fast, hermetic checks that do NOT run a full mutation sweep on the cosmon
# workspace (that is minutes-to-hours, reserved for the nightly cron / periodic
# formula). Exercises:
#   1. --check availability probe returns a sane code (0 if installed, 2 if not);
#   2. an unknown argument → exit 2;
#   3. the empty-diff fast path → exit 0 without invoking cargo-mutants;
#   4. the built-in --self-test — a REAL bounded mutation proof (a hole crate
#      reddens, a well-tested crate stays green) when cargo-mutants is present;
#      gracefully SKIPs (exit 0) when it is absent.
#
# Usage: ./scripts/mutation-falsifier.test.sh
# Exit: 0 on pass, non-zero on failure.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GATE="$HERE/mutation-falsifier.sh"

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

pass=0 fail=0
ok()  { echo "PASS: $*"; pass=$((pass+1)); }
bad() { echo "FAIL: $*" >&2; fail=$((fail+1)); }

have_mutants() { command -v cargo-mutants >/dev/null 2>&1 || cargo mutants --version >/dev/null 2>&1; }

# ── 1. --check availability probe ────────────────────────────────────────────
bash "$GATE" --check >/dev/null 2>&1; rc=$?
if have_mutants; then
  [ "$rc" -eq 0 ] && ok "--check → 0 (cargo-mutants installed)" || bad "--check should be 0 when installed, got $rc"
else
  [ "$rc" -eq 2 ] && ok "--check → 2 (cargo-mutants absent)" || bad "--check should be 2 when absent, got $rc"
fi

# ── 2. unknown argument → exit 2 ─────────────────────────────────────────────
bash "$GATE" --nonsense-flag >/dev/null 2>&1; rc=$?
[ "$rc" -eq 2 ] && ok "unknown arg → exit 2" || bad "unknown arg should be exit 2, got $rc"

# ── 3. empty-diff fast path → exit 0, no cargo-mutants run ────────────────────
# Only meaningful when cargo-mutants is present (else the availability guard
# short-circuits to exit 2 before the diff check — which is correct behaviour).
if have_mutants; then
  REPO="$WORK/repo"
  mkdir -p "$REPO"
  cd "$REPO" || exit 2
  git init -q
  git config user.email "test@example.com"
  git config user.name "test"
  echo "seed" > seed.txt
  git add -A && git commit -qm "seed"
  git branch -f main HEAD >/dev/null 2>&1 || true
  # HEAD == main, so the diff vs main is empty → fast path exit 0.
  bash "$GATE" --base main >/dev/null 2>&1; rc=$?
  [ "$rc" -eq 0 ] && ok "empty diff vs base → exit 0 (fast path)" || bad "empty diff should be exit 0, got $rc"
  cd "$HERE" || exit 2
else
  echo "SKIP: empty-diff path needs cargo-mutants (absent)"
fi

# ── 4. built-in --self-test (real bounded mutation proof, or graceful SKIP) ──
if bash "$GATE" --self-test; then ok "--self-test passes (or SKIPs cleanly)"
else bad "--self-test should pass"; fi

# A fake cargo records the actual boundary arguments and leaves independent
# outcome fixtures. This checks forwarding and failure reporting without a
# workspace mutation sweep.
mkdir -p "$WORK/bin"
cat > "$WORK/bin/cargo" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$MF_ARGS"
[ "${2:-}" = --version ] && { echo 'cargo-mutants fixture'; exit 0; }
while [ $# -gt 0 ]; do
  if [ "$1" = -o ]; then out="$2"; break; fi
  shift
done
[ "$MF_MODE" = missing ] && exit 4
mkdir -p "$out/mutants.out"
: > "$out/mutants.out/missed.txt"
[ "$MF_MODE" = missed ] && echo 'independent surviving mutant' > "$out/mutants.out/missed.txt"
echo 'fixture progress and timing'
[ "$MF_MODE" = failed ] && exit 4
[ "$MF_MODE" = missed ] && exit 2
exit 0
SH
chmod +x "$WORK/bin/cargo"
for mode in clean missed failed missing; do
  MF_MODE="$mode" MF_ARGS="$WORK/args" PATH="$WORK/bin:$PATH" \
    bash "$GATE" --whole-tree --package cosmon-core --shard 3/256 \
      --build-timeout 180 --output "$WORK/$mode" > "$WORK/$mode.log" 2>&1
  rc=$?
  case "$mode" in clean) expected=0 ;; missed) expected=1 ;; *) expected=2 ;; esac
  [ "$rc" -eq "$expected" ] && ok "$mode report → $expected" || bad "$mode report exit $rc"
done
python3 - "$WORK/args" <<'PYTEST'
import sys
args = open(sys.argv[1]).read().splitlines()
for option, value in [("--shard", "3/256"), ("--build-timeout", "180"), ("-p", "cosmon-core")]:
    assert args[args.index(option) + 1] == value, args
PYTEST
[ $? -eq 0 ] && ok "shard, build budget and package reach cargo-mutants" || bad "lost arguments"
[ -s "$WORK/missed/mutants.out/missed.txt" ] && \
  grep -q 'independent surviving mutant' "$WORK/missed.log" && \
  grep -q 'fixture progress and timing' "$WORK/missed/cargo-mutants.log" && \
  ok "survivor and timing reports retained" || bad "lost report"
for shard in 1/1 0/0 invalid; do
  bash "$GATE" --whole-tree --shard "$shard" > /dev/null 2>&1
  [ $? -eq 2 ] && ok "reject invalid shard $shard" || bad "accepted invalid shard $shard"
done

# Execute the workflow's actual shell body with boundary stubs across an
# entire rotation. This catches missing/duplicated shards, a lost deadline,
# or matrix fail-fast cancelling its siblings.
python3 - "$HERE/../.github/workflows/ci.yml" "$WORK/rotation" <<'PYTEST'
import os
from pathlib import Path
import subprocess
import sys
import yaml

job = yaml.safe_load(Path(sys.argv[1]).read_text())["jobs"]["mutation-falsifier"]
assert job["continue-on-error"] is True
assert job["if"] == "github.event_name == 'schedule'"
assert job["timeout-minutes"] == 180
assert job["strategy"]["fail-fast"] is False
slots = job["strategy"]["matrix"]["slot"]
assert slots == list(range(8))
assert job["strategy"]["max-parallel"] <= 8
step = next(s for s in job["steps"] if s.get("env", {}).get("SHARD_SLOT"))
root = Path(sys.argv[2])
(root / "scripts").mkdir(parents=True)
(root / "bin").mkdir()
files = {
    "bin/date": '#!/bin/sh\necho "$EPOCH_SECONDS"\n',
    "bin/timeout": '#!/bin/sh\n[ "$1 $2 $3" = "--signal=TERM --kill-after=30s 150m" ] || exit 9\nshift 3\nexec "$@"\n',
    "scripts/mutation-falsifier.sh": '#!/bin/sh\nprintf "%s\\n" "$@" >> "$CAPTURE"\n',
}
for name, body in files.items():
    path = root / name
    path.write_text(body)
    path.chmod(0o755)
capture = root / "args"
for day in range(31, 63):  # includes modulo wrap at day 32
    for slot in slots:
        env = dict(os.environ, PATH=str(root / "bin") + ":" + os.environ["PATH"],
                   EPOCH_SECONDS=str(day * 86400), SHARD_SLOT=str(slot), CAPTURE=str(capture))
        subprocess.run(["bash", "-eu", "-c", step["run"]], cwd=root, env=env,
                       check=True, stdout=subprocess.DEVNULL)
args = capture.read_text().splitlines()
shards = [args[i + 1] for i, arg in enumerate(args) if arg == "--shard"]
assert sorted(shards) == sorted(f"{k}/256" for k in range(256))
# A fixed source inventory is partitioned completely, exactly once, using
# the selected round-robin algorithm; the boundary must request it.
assert args.count("round-robin") == 256
assert args.count("cosmon-core") == 256
assert args.count("--whole-tree") == 256
assert args.count("--build-timeout") == 256
assert args.count("--no-shuffle") == 256
PYTEST
[ $? -eq 0 ] && ok "workflow covers all 256 shards in 32 nights with an inner deadline" || bad "workflow rotation or advisory budget"

echo "────────────────────────────────────────────"
echo "mutation-falsifier.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
