#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Deterministic reproduction of the ADR-161 containment OVER-REFUSAL
# (task-20260724-07bc; re-measured as repro-20260806-8db2).
#
# THE CONTRACT UNDER TEST
#   In a repo whose root holds `.cosmon/config.toml`, with a spore definition
#   under `spores/<name>/`, running
#
#       cd spores/<name> && cs spore run spore-starter.toml --allow-unchecked-seal
#
#   must germinate. The run-scoped output home ADR-161 hands each node is
#   `<repo>/.cosmon/state/spore-runs/<germination-id>/<alias>/` — under the
#   project state store, NOT inside the spore definition tree. Refusing it as
#   `InsideSporeDefinition` is a FALSE POSITIVE of the containment guard.
#
# THE SEAM AND THE PRE-ORACLE DECISION
#   The seam is `cosmon_core::spore::forbidden_gate_output(out, manifest_dir,
#   repo_root)`, called from `run_run` in `crates/cosmon-cli/src/cmd/spore.rs`
#   BEFORE the seal gate and before any state is written. The pre-oracle
#   decision is that guard's verdict on the handed `output_dir` — pure lexical
#   path arithmetic, no clock, no network, no model. This harness reads that
#   decision at the CLI seam, through the refusal string the guard produces.
#   No model, no network, no oracle is consulted: `--allow-unchecked-seal`
#   skips TLC, and germination writes molecules to disk without dispatching a
#   worker.
#
# THE FOUR ARMS (the differential refutation is built in)
#   R  relative bare  `spore-starter.toml`     -> must germinate
#   D  dot-relative   `./spore-starter.toml`   -> must germinate
#   A  absolute       `<abs>/spore-starter.toml` -> must germinate (CONTROL)
#   V  validate       `cs spore validate spore-starter.toml` -> must succeed
#
#   Exactly ONE variable separates R/D from A: the FORM of the manifest
#   reference. Everything else — the fixture, the spore, the run home, the
#   cwd — is identical. A defect that shows in R/D while A stays green is the
#   guard mis-reading a relative manifest dir; a failure in ALL FOUR arms is a
#   broken fixture, not this defect, and the harness reports it as such.
#
# EXIT CODES (this harness is a TEST: green when the defect is absent)
#   0  DEFECT ABSENT   — all four arms behave per contract.
#   1  DEFECT PRESENT  — R and/or D refused with InsideSporeDefinition while
#                        the absolute control A germinated. The reproduction.
#   2  HARNESS FAULT   — the fixture or the environment is wrong (named
#                        false-red mode). NOT evidence about the defect.
#
# USAGE
#   repro-adr161-relative-manifest.sh /path/to/cs
#
# It is DELIBERATELY not wired into `cargo test --workspace`: it is red by
# design on the affected ref, and reddening the repo's own gates for that would
# be a false-red on every unrelated branch. The clean-room runs it against a
# `git archive <affected_ref>` checkout instead.

set -u
set -o pipefail

CS="${1:-}"
if [[ -z "$CS" || ! -x "$CS" ]]; then
    echo "HARNESS FAULT: usage: $0 /path/to/cs (got '${CS}')" >&2
    exit 2
fi
# Every arm runs with the cwd inside the spore dir, so a RELATIVE `cs` path
# would resolve to nothing there and every arm would fail with rc=127 — a
# false-red that says nothing about the guard. Anchor it once, up front.
CS="$(cd "$(dirname "$CS")" && pwd)/$(basename "$CS")"

# FALSE-GREEN GUARD 1: an inherited COSMON_STATE_DIR would send the run home
# somewhere other than `<repo>/.cosmon/state`, so the guard would compare a
# different pair of paths than the reported symptom and could pass for a
# reason unrelated to the defect. Neutralise it.
unset COSMON_STATE_DIR

WORK="$(mktemp -d "${TMPDIR:-/tmp}/repro-adr161.XXXXXX")" || {
    echo "HARNESS FAULT: cannot create a work dir" >&2
    exit 2
}
trap 'rm -rf "$WORK"' EXIT

REPO="$WORK/host-repo"
SPORE_DIR="$REPO/spores/starter"
mkdir -p "$REPO/.cosmon" "$SPORE_DIR/formulas" || exit 2

# A cosmon PROJECT root is `.cosmon/config.toml` as a regular file (ADR-069);
# a bare `.cosmon/` directory is a user-level state host and walk-up discovery
# steps past it. Without this file the run home resolves to the operator's
# GLOBAL `~/.cosmon/state` — which both pollutes their store and measures a
# different path pair than the report. Guard 2 below proves it landed right.
cat > "$REPO/.cosmon/config.toml" <<'EOF'
[project]
project_id = "repro-adr161"
trunk_branch = "main"
EOF

cat > "$SPORE_DIR/spore-starter.toml" <<'EOF'
[spore]
name = "starter"
version = 1

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "decompose"
kind = "fixed"
formula = "work"
[spore.node.vars]
topic = "Write ${output_dir}/verdict.json"
EOF

cat > "$SPORE_DIR/formulas/work.formula.toml" <<'EOF'
formula = "work"
version = 1
description = "The smallest leaf formula that germinates — fixture only."
id_prefix = "task"

[vars.topic]
description = "The task, one sentence."
required = true

[[steps]]
id = "do"
title = "Do the thing"
description = "Do the thing named by the topic."
acceptance = "The thing is done."
EOF

# ---------------------------------------------------------------------------
# One arm: run `cs` from inside the spore dir and classify the outcome.
# Prints "<rc>|<classification>" and leaves the captured output in $OUT_FILE.
# ---------------------------------------------------------------------------
arm() {
    local name="$1"
    shift
    OUT_FILE="$WORK/arm-$name.out"
    ( cd "$SPORE_DIR" && "$CS" "$@" ) > "$OUT_FILE" 2>&1
    local rc=$?

    if [[ $rc -eq 0 ]]; then
        echo "$rc|GERMINATED"
        return
    fi
    # RIGHT-REASON DISCIPLINE: a non-zero exit is not the defect. Only the
    # containment guard's own refusal, naming InsideSporeDefinition, is.
    if grep -q 'forbidden output home' "$OUT_FILE" \
       && grep -q 'InsideSporeDefinition' "$OUT_FILE"; then
        echo "$rc|REFUSED_INSIDE_SPORE_DEFINITION"
        return
    fi
    echo "$rc|OTHER_FAILURE"
}

echo "== repro-adr161-relative-manifest =="
echo "cs      : $CS"
echo "version : $("$CS" --version 2>&1 | head -1)"
echo "fixture : $REPO"
echo

declare -A RESULT
for spec in \
    "R:spore run spore-starter.toml --allow-unchecked-seal" \
    "D:spore run ./spore-starter.toml --allow-unchecked-seal" \
    "A:spore run $SPORE_DIR/spore-starter.toml --allow-unchecked-seal" \
    "V:spore validate spore-starter.toml"
do
    name="${spec%%:*}"
    # shellcheck disable=SC2086
    RESULT[$name]="$(arm "$name" ${spec#*:})"
    echo "arm $name : ${RESULT[$name]}"
    sed 's/^/         | /' "$WORK/arm-$name.out"
    echo
done

# ---------------------------------------------------------------------------
# FALSE-GREEN GUARD 2: prove the run home actually landed under the FIXTURE's
# `.cosmon/state`. A germination that silently used the global store would be
# a green about a path pair the report never described.
# ---------------------------------------------------------------------------
if [[ ! -d "$REPO/.cosmon/state/spore-runs" ]]; then
    if [[ "${RESULT[A]}" == *GERMINATED* ]]; then
        echo "HARNESS FAULT: the absolute control germinated but no run home" >&2
        echo "  exists under $REPO/.cosmon/state/spore-runs — the state dir" >&2
        echo "  resolved somewhere else, so this run measures the wrong paths." >&2
        exit 2
    fi
fi

# ---------------------------------------------------------------------------
# FALSE-RED GUARD: the absolute control is the discriminator. If A itself
# fails, the fixture/binary is wrong and NOTHING here is evidence about the
# relative-reference defect.
# ---------------------------------------------------------------------------
if [[ "${RESULT[A]}" != *GERMINATED* ]]; then
    echo "HARNESS FAULT: the absolute control (arm A) did not germinate:" >&2
    echo "  ${RESULT[A]}" >&2
    echo "  The fixture or the binary is broken; the relative arms say nothing." >&2
    exit 2
fi

# `validate` is reported unaffected. If it refuses, the defect is wider than
# reported — still a finding, but not the one this red is frozen on.
if [[ "${RESULT[V]}" == *OTHER_FAILURE* ]]; then
    echo "HARNESS FAULT: arm V (validate) failed for an unrelated reason:" >&2
    echo "  ${RESULT[V]}" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# THE VERDICT.
# ---------------------------------------------------------------------------
reproduced=0
for name in R D; do
    if [[ "${RESULT[$name]}" == *REFUSED_INSIDE_SPORE_DEFINITION* ]]; then
        reproduced=1
    elif [[ "${RESULT[$name]}" == *OTHER_FAILURE* ]]; then
        echo "HARNESS FAULT: arm $name failed for a reason that is NOT the" >&2
        echo "  containment refusal: ${RESULT[$name]}" >&2
        exit 2
    fi
done

if [[ $reproduced -eq 1 ]]; then
    echo "RESULT: DEFECT PRESENT"
    echo "  A relative manifest reference is refused as InsideSporeDefinition"
    echo "  while the SAME spore under an ABSOLUTE reference germinates."
    echo "  R=${RESULT[R]}  D=${RESULT[D]}  A=${RESULT[A]}  V=${RESULT[V]}"
    exit 1
fi

echo "RESULT: DEFECT ABSENT"
echo "  R=${RESULT[R]}  D=${RESULT[D]}  A=${RESULT[A]}  V=${RESULT[V]}"
exit 0
