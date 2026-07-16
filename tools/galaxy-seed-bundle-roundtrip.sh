#!/usr/bin/env bash
# galaxy-seed-bundle-roundtrip.sh
#
# Falsifiable hard-proof (smithy G2 / ADR-0023 §D4) that a galaxy's
# `galaxy-seed` = BLAKE3(genesis event) is invariant across a real
# bundle/sneakernet clone, simulating: Dave bundles a galaxy → sneakernet
# → Casey reconstructs it → both recompute the seed.
#
# It does the transport FOR REAL with `git bundle` (the actual sneakernet
# carrier), on REAL genesis lines pulled from live galaxy ledgers — no mocks.
#
# Usage:  tools/galaxy-seed-bundle-roundtrip.sh [GALAXIES_ROOT]
#   GALAXIES_ROOT defaults to ~/galaxies
#
# Exit 0 = every tested galaxy's seed survives the round-trip (PASS).
# Exit 1 = a divergence was found (FAIL — galaxy-seed is NOT clone-stable).
set -euo pipefail

GALAXIES_ROOT="${1:-$HOME/galaxies}"
GALAXIES=(cosmon smithy noogram lumen speck)

# --- pick a BLAKE3 CLI; fall back to python's hashlib if no `b3sum` ---
b3() {
  if command -v b3sum >/dev/null 2>&1; then
    b3sum | awk '{print $1}'
  else
    # `-c` keeps the piped data on stdin (a heredoc would shadow it).
    python3 -c 'import sys,hashlib
try: h=hashlib.blake3(sys.stdin.buffer.read())
except AttributeError: sys.exit("no blake3 available (need b3sum or python blake3)")
print(h.hexdigest())'
  fi
}

# emit RFC-8785-style canonical JSON (sorted keys, no whitespace) for a
# genesis line. Mirrors cosmon_hash::canonical_serialize for the int+string
# value space of genesis events.
canon_bytes() {
  # NB: use `python3 -c` (not `python3 - <<HEREDOC`) so that the DATA piped
  # in stays on stdin — a heredoc would itself become stdin and shadow it.
  python3 -c 'import sys,json; v=json.loads(sys.stdin.buffer.read()); sys.stdout.write(json.dumps(v,sort_keys=True,separators=(",",":"),ensure_ascii=False))'
}

# canonical seed = BLAKE3 of canonical JSON. Hashed through the same b3()
# helper so shell proof and Rust referent agree bit-for-bit.
canon_seed() { canon_bytes | b3; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fail=0
printf "%-10s %-8s %-18s %-18s %s\n" GALAXY VERDICT "RAW-SEED(8)" "CANON-SEED(8)" NOTE
printf '%.0s-' {1..78}; echo

for g in "${GALAXIES[@]}"; do
  src="$GALAXIES_ROOT/$g/.cosmon/state/events.jsonl"
  if [[ ! -f "$src" ]]; then
    printf "%-10s %-8s %s\n" "$g" SKIP "no events.jsonl"
    continue
  fi

  # ---- DAVE side: stage the genesis line into a git repo, bundle it ----
  dave="$WORK/$g/dave"; mkdir -p "$dave"
  head -1 "$src" > "$dave/genesis.jsonl"      # genesis event, byte-exact
  git -C "$dave" init -q
  git -C "$dave" -c user.email=m@x -c user.name=dave add genesis.jsonl
  git -C "$dave" -c user.email=m@x -c user.name=dave commit -q -m genesis
  git -C "$dave" bundle create -q "$WORK/$g/speck.bundle" --all

  # ---- SNEAKERNET: only the .bundle file crosses (copy to a cold dir) ----
  carrier="$WORK/$g/usb"; mkdir -p "$carrier"
  cp "$WORK/$g/speck.bundle" "$carrier/"

  # ---- JESSE side: clone from the bundle, reconstruct genesis line ----
  casey="$WORK/$g/casey"
  git clone -q "$carrier/speck.bundle" "$casey"

  raw_m=$(b3 < "$dave/genesis.jsonl")
  raw_j=$(b3 < "$casey/genesis.jsonl")
  can_m=$(canon_seed < "$dave/genesis.jsonl")
  can_j=$(canon_seed < "$casey/genesis.jsonl")

  note="raw=eq canon=eq"
  verdict=PASS
  if [[ "$raw_m" != "$raw_j" ]]; then note="RAW DIVERGED"; verdict=FAIL; fail=1; fi
  if [[ "$can_m" != "$can_j" ]]; then note="CANON DIVERGED"; verdict=FAIL; fail=1; fi

  printf "%-10s %-8s %-18s %-18s %s\n" \
    "$g" "$verdict" "${raw_j:0:8}…" "${can_j:0:8}…" "$note"
done

printf '%.0s-' {1..78}; echo
if [[ "$fail" == 0 ]]; then
  echo "RESULT: PASS — galaxy-seed is invariant across bundle/sneakernet clone."
else
  echo "RESULT: FAIL — at least one galaxy's seed diverged across the clone."
fi
exit "$fail"
