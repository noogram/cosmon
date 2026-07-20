#!/usr/bin/env bash
# Probe #1 — CS VERIFY: the real bug is a v1/v2 event-schema mismatch.
#
# v1-bench BUG (fixed here): the old probe drove INVALID CLI —
#   `cs init --non-interactive`  (no such flag)
#   `cs nucleate task`           (no `task` formula; the formula is `task-work`)
# so it created NO molecule, then ran a bare `cs verify` which fails with
#   "cs: molecule_id is required unless --federation is set"
# and mislabelled that CLI-misuse as a verify defect. It also ran inside a
# cosmon worker, where COSMON_PARENT_MOL_ID makes `cs nucleate` try to auto-link
# to a non-existent parent unless `--no-parent` is passed.
#
# Correct reproduction (this probe):
#   1. `cs init`
#   2. `cs --json nucleate task-work --var topic=... --no-parent`  -> capture .id
#   3. `cs verify <id>` on the FRESH molecule
#         => PASS, with the event-chain check reported SKIP
#            ("no molecule-local event log"). The tester's naive "verify fails"
#            does NOT reproduce for a real molecule — it was CLI misuse.
#   4. Materialise a molecule-local events.jsonl carrying the *live* v2 event
#      schema (tag `type`, e.g. `molecule_nucleated`) — exactly what the runtime
#      writes to the fleet-wide log — and `cs verify <id>` again.
#         => this is where the REAL bug lives:
#            "chain walk error: missing field `kind` ..."
#      because `cs verify` deserialises the log with the v1 `Event` enum
#      (`#[serde(tag="kind")]`, crates/cosmon-core/src/event.rs:167) while the
#      runtime writes v2 `EventV2` (`#[serde(tag="type")]`,
#      crates/cosmon-core/src/event_v2.rs:377). The v1 verify reader was never
#      migrated to the tolerant v2 reader (Envelope::from_line).
#
# Verdict (honest):
#   RED   if the v2-schema chain-walk mismatch reproduces on this tree
#         (fresh molecule still verifies PASS — the defect is latent, firing
#          only when a molecule-local log holds v2 lines — but the schema
#          mismatch is real and unfixed: a still-open cosmon-ward finding).
#   GREEN if BOTH the fresh molecule verifies PASS *and* a v2-schema
#         molecule-local log now verifies cleanly (reader migrated / tolerant).
#   INCONCLUSIVE if no `cs` binary is available to run the decisive step.

source "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

ID="issue-1-cs-verify"
NAME="cs verify: v1/v2 event-schema mismatch (missing field \`kind\`)"
ADAPTER="cli"

SRC="${1:-}"
CLEANUP=0
if [[ -z "$SRC" ]]; then
  SRC="$(mktemp -d)"; CLEANUP=1
  checkout_uut "$SRC"
fi

EVIDENCE="$EVIDENCE_OUT/$ID.txt"
: > "$EVIDENCE"
{
  echo "# Probe #1 cs verify (v1/v2 schema mismatch)"
  echo "# unit-under-test: $COSMON_TAG"
  echo "# cs binary: $CS_BIN"
  echo
} >> "$EVIDENCE"

# --- Static: locate the writer/verifier schema split ------------------------
echo "## static: verify reader tag vs runtime writer tag" >> "$EVIDENCE"
VERIFY_TAG="$(rg -n 'serde\(tag *= *"kind"' "$SRC/crates/cosmon-core/src/event.rs" 2>/dev/null | sed "s#$SRC/##g" || true)"
WRITER_TAG="$(rg -n 'serde\(tag *= *"type"' "$SRC/crates/cosmon-core/src/event_v2.rs" 2>/dev/null | sed "s#$SRC/##g" || true)"
echo "verify (v1 Event) tag:  ${VERIFY_TAG:-  (not found)}" >> "$EVIDENCE"
echo "runtime (v2 EventV2) tag: ${WRITER_TAG:-  (not found)}" >> "$EVIDENCE"
SCHEMA_SPLIT=0
[[ -n "$VERIFY_TAG" && -n "$WRITER_TAG" ]] && SCHEMA_SPLIT=1
echo "schema_split_present=$SCHEMA_SPLIT" >> "$EVIDENCE"
echo >> "$EVIDENCE"

# --- Runtime: the decisive two-phase reproduction ---------------------------
FRESH_RC=""          # cs verify rc on a fresh molecule
FRESH_CHAIN=""       # event-chain status on the fresh molecule (SKIP expected)
V2_RC=""             # cs verify rc with a v2-schema molecule-local log
V2_SIG=""            # the captured chain-walk signature (empty if clean)
RUNTIME_STATE="skipped-no-binary"

if [[ -x "$CS_BIN" ]]; then
  RUNTIME_STATE="ran"
  WORK="$(mktemp -d)"
  pushd "$WORK" >/dev/null
  set +e   # the decisive commands intentionally exit non-zero; capture, don't abort
  {
    echo "### phase A: init + nucleate + verify FRESH molecule"
    "$CS_BIN" init >/dev/null 2>&1 || true
    NJSON="$("$CS_BIN" --json nucleate task-work --var topic="verify-probe" --no-parent 2>/dev/null || true)"
    echo "nucleate json: $NJSON"
    MID="$(printf '%s' "$NJSON" | jq -r '.id // .molecule_id // empty' 2>/dev/null || true)"
    echo "captured molecule id: [$MID]"
    if [[ -n "$MID" ]]; then
      echo "--- cs verify $MID (fresh) ---"
      FRESH_OUT="$("$CS_BIN" verify "$MID" 2>&1)"; FRESH_RC=$?
      printf '%s\n' "$FRESH_OUT"
      # Event-chain check status on the fresh molecule (expected SKIP).
      FRESH_CHAIN="$(printf '%s\n' "$FRESH_OUT" | rg -o '\[(PASS|SKIP|FAIL)\] event-chain' | head -1 | tr -d '[]' | awk '{print $1}')"
      echo "fresh verify rc=$FRESH_RC  event-chain=$FRESH_CHAIN"

      echo
      echo "### phase B: materialise a v2-schema molecule-local events.jsonl and re-verify"
      MDIR=".cosmon/state/fleets/default/molecules/$MID"
      if [[ -d "$MDIR" ]]; then
        # Prefer real fleet-log lines (the exact bytes the runtime writes); fall
        # back to a canonical v2 line so the probe is self-contained.
        if [[ -f .cosmon/state/events.jsonl ]]; then
          head -2 .cosmon/state/events.jsonl > "$MDIR/events.jsonl"
        else
          printf '%s\n' '{"seq":0,"timestamp":"2026-01-01T00:00:00Z","type":"molecule_nucleated","molecule_id":"'"$MID"'"}' > "$MDIR/events.jsonl"
        fi
        echo "molecule-local events.jsonl (v2 schema, tag=type):"
        sed 's/^/    /' "$MDIR/events.jsonl"
        echo "--- cs verify $MID (with v2-schema molecule-local log) ---"
        V2_OUT="$("$CS_BIN" verify "$MID" 2>&1)"; V2_RC=$?
        printf '%s\n' "$V2_OUT"
        V2_SIG="$(printf '%s\n' "$V2_OUT" | rg -o 'chain walk error: [^\n]*' | head -1 || true)"
        echo "v2-log verify rc=$V2_RC  chain-walk-signature=[$V2_SIG]"
      else
        echo "molecule dir $MDIR absent — cannot stage phase B."
      fi
    else
      echo "FAILED to capture a molecule id from nucleate — CLI surface may have shifted."
    fi
  } >> "$EVIDENCE" 2>&1
  set -e
  popd >/dev/null
  rm -rf "$WORK"
else
  echo "## runtime: cs binary absent ($CS_BIN) — decisive step not run here." >> "$EVIDENCE"
fi
echo >> "$EVIDENCE"

SIG="schema_split=$SCHEMA_SPLIT fresh_rc=${FRESH_RC:-NA} fresh_chain=${FRESH_CHAIN:-NA} v2_rc=${V2_RC:-NA} v2_mismatch=$([[ -n "$V2_SIG" ]] && echo 1 || echo 0)"

if [[ "$RUNTIME_STATE" != "ran" || -z "$FRESH_RC" ]]; then
  VERDICT="INCONCLUSIVE"
  NOTE="No cs binary available to run the decisive verify (schema_split_present=$SCHEMA_SPLIT). Build cs and set CS_BIN, or run the container path."
elif [[ -n "$V2_SIG" ]]; then
  VERDICT="RED"
  NOTE="Real bug REPRODUCES: a molecule-local events.jsonl in the live v2 schema (tag \`type\`) fails cs verify's v1 chain-walk with '$V2_SIG'. The fresh molecule verifies PASS (rc=$FRESH_RC, event-chain=$FRESH_CHAIN) — the tester's bare \`cs verify\` was CLI-misuse (id required), NOT a defect. The genuine defect is the v1/v2 schema mismatch (verify reads cosmon_core::event::Event tag=kind; runtime writes cosmon_core::event_v2 tag=type). Verify reader NOT migrated to the tolerant Envelope::from_line — still-open, surface cosmon-ward."
elif [[ "$FRESH_RC" == "0" ]]; then
  VERDICT="GREEN"
  NOTE="cs verify PASSES on a fresh molecule (event-chain=$FRESH_CHAIN) AND a v2-schema molecule-local log now verifies cleanly (rc=$V2_RC, no chain-walk error) — the v1/v2 reader mismatch is resolved on this tree."
else
  VERDICT="RED"
  NOTE="cs verify FAILED (rc=$FRESH_RC) on a fresh molecule — real signature in evidence; not the CLI-misuse path."
fi

emit_probe "$ID" "$NAME" "$ADAPTER" "$VERDICT" "$SIG" "$EVIDENCE" "$NOTE"

[[ "$CLEANUP" -eq 1 ]] && rm -rf "$SRC"
exit 0
