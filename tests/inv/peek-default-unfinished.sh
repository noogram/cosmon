#!/usr/bin/env bash
# Witness for INV-PEEK-DEFAULT-UNFINISHED (delib-20260716-a2f1 §C4).
#
# `cs peek` with no phase flag surfaces every molecule whose story is
# not over. The predicate is `!terminal`, never `== running`.
#
# This witness REPLACES `peek-default-running-only.sh`, which guarded
# the defect rather than the contract. The operator asked to hide the
# archive; the code hid everything that was not `running`, and the INV
# pinned that reading in place — so the five frozen and twenty-seven
# orphaned molecules stayed invisible with a green gate over them. An
# instrument may hide what it has already told you; it may never hide
# what it has not.
#
# This INV protects against four regressions:
#   1. The default narrows back to `running`, by any spelling.
#   2. `Starved` (ADR-062 — alive, and the one status whose purpose is
#      to summon the operator) is filed with the archive again.
#   3. A wildcard arm reappears in `MoleculeStatus::phase`, letting a
#      new status be silently mis-bucketed instead of breaking the build.
#   4. `--all` stops meaning all.
#
# The witness greps the source for the contract and short-circuits at
# the first failure. Sources of truth:
#   crates/cosmon-core/src/molecule.rs  (Phase, MoleculeStatus::phase)
#   crates/cosmon-cli/src/cmd/peek.rs   (PhaseFilter)
set -uo pipefail

INV="INV-PEEK-DEFAULT-UNFINISHED"
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$SCRIPT_DIR/../.."
PEEK_RS="$ROOT/crates/cosmon-cli/src/cmd/peek.rs"
MOLECULE_RS="$ROOT/crates/cosmon-core/src/molecule.rs"

if [[ ! -f "$PEEK_RS" || ! -f "$MOLECULE_RS" ]]; then
  echo "$INV: SKIP — peek.rs / molecule.rs not found (wrong galaxy?)" >&2
  exit 0
fi

fail() {
  echo "$INV: FAIL — $1" >&2
  exit 1
}

# (1) The codomain exists in the core, beside the status enum.
grep -qE '^pub enum Phase' "$MOLECULE_RS" \
  || fail "cosmon_core::molecule::Phase is missing — the operator-facing
     category of a molecule must be a named type, not five hand-written
     tables that are each free to disagree"

# (2) `phase()` is total: no wildcard arm may reappear inside it. A `_ =>`
# here is what let a new status silently mis-render in six places.
phase_body="$( awk '/pub fn phase\(self\) -> Phase/,/^    }$/' "$MOLECULE_RS" )"
[[ -n "$phase_body" ]] \
  || fail "could not locate MoleculeStatus::phase() in molecule.rs"
if grep -qE '^\s*_\s*=>' <<<"$phase_body"; then
  echo "$INV: FAIL — MoleculeStatus::phase() has a wildcard arm." >&2
  echo "  #[non_exhaustive] is a promise downstream, never a shield" >&2
  echo "  upstream: adding a status must break the build at exactly" >&2
  echo "  this site, so the author names its phase in the same commit." >&2
  echo "---- offending body ----" >&2
  echo "$phase_body" >&2
  exit 1
fi

# (3) Starved bands as Blocked — alive. ADR-062: wait or rotate, never a
# re-prompt. It is the one status that exists to summon the operator, and
# every classification in peek used to file it with 917 corpses.
grep -qE 'Self::Starved\s*=>\s*Phase::Blocked' <<<"$phase_body" \
  || fail "Starved must phase to Blocked (ADR-062 — it is alive)"

# (4) The default is the unfinished set, and it is exactly !terminal.
unfinished_body="$( awk '/pub const fn unfinished\(\)/,/^    }$/' "$PEEK_RS" )"
[[ -n "$unfinished_body" ]] \
  || fail "PhaseFilter::unfinished() is missing from peek.rs"
for phase in Live Waiting Blocked Parked; do
  grep -qE "with\(Phase::${phase}\)" <<<"$unfinished_body" \
    || fail "PhaseFilter::unfinished() drops Phase::${phase} — the default
     must be !terminal, not a narrower set wearing the name"
done
for phase in Failed Done; do
  if grep -qE "with\(Phase::${phase}\)" <<<"$unfinished_body"; then
    fail "PhaseFilter::unfinished() includes Phase::${phase} — the archive
     is the one thing the default is meant to hide"
  fi
done

# (5) The default of the type is the unfinished set, not a narrower one.
default_body="$( awk '/impl Default for PhaseFilter/,/^}$/' "$PEEK_RS" )"
grep -qE 'Self::unfinished\(\)' <<<"$default_body" \
  || fail "PhaseFilter::default() must be unfinished()"

# (6) The legacy three-boolean struct stays dead. It had eight
# representable states, labelled all eight, and could reach exactly four —
# `running` was hardcoded true on every path.
if grep -qE 'pub struct StateFilter|pub const fn default_watchdog' "$PEEK_RS"; then
  fail "the StateFilter/default_watchdog surface is back. Its booleans
     were predicates over a domain with no name; the domain is now
     Phase, and the filter is a set over it"
fi

# (7) One chokepoint resolves the CLI flags into the filter.
grep -qE 'fn phase_filter\(' "$PEEK_RS" \
  || fail "Args::phase_filter() helper is missing"

# (8) --all means all. The panel was unanimous that a flag named --all
# returning something that is not all is the one unforgivable move: the
# operator can never again trust any peek output once they know the tool
# has opinions about what they meant.
all_body="$( awk '/pub const fn all\(\)/,/^    }$/' "$PEEK_RS" )"
grep -qE '0b0011_1111' <<<"$all_body" \
  || fail "PhaseFilter::all() no longer sets every phase bit"

echo "$INV: PASS (default is !terminal; phase() total with no wildcard; Starved alive; --all means all)"
exit 0
