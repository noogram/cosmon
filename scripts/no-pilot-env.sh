#!/usr/bin/env bash
# no-pilot-env.sh — run a verification gate OUTSIDE the worker's pilotage
# environment, then exec it.
#
# `cs tackle` steers a worker by putting variables in its environment: which
# Claude account to bill, how deep the spawn chain is, where the molecule state
# lives, whether the adapter may reach the network. The worker then runs
# `just gates`, and `cargo test` inherits all of it — so every test process
# reads instructions addressed to its parent.
#
# Three times that produced a false verdict, always the same shape (a PILOTAGE
# variable read as a CONFIGURATION variable by a test):
#
#   COSMON_EGRESS_POLICY=deny-external  → the cs-spawning HTTP suites. On
#     2026-08-06 this collapsed a healthy molecule, task-20260804-2bbb; the
#     work was intact (preserved at 226b9b0d) and the verdict was false.
#   CB_DEPTH                            → every `cs tackle` a test spawns hits
#     the depth guard: 4 tackle suites + 7 others red, 1588 passed / 0 failed
#     once removed.
#   ANTHROPIC_MODEL                     → credential suites, via the F6
#     incoherent-pair refusal.
#
# WHAT THIS IS NOT. This is not a way to relax egress confinement. A local
# adapter running under `deny-external` is a real security guarantee and this
# script must never appear on that path — it belongs to the `justfile` and CI
# and to nothing else, which `boundary_is_never_applied_to_a_runtime_path`
# pins. The fix for a test poisoned by the policy is to keep the test out of
# the adapter's environment, never to weaken the adapter.
#
# THE LIST BELOW IS NOT THE SOURCE OF TRUTH. `crates/cosmon-core/src/pilot_env.rs`
# is: `cs tackle` emits every variable through `PilotVar::name()`, so a
# variable that does not exist there cannot be injected at all. This file is a
# projection of that enum for the shell, and `pilot_env_boundary.rs` fails if
# the two ever disagree — which is what stops this list rotting silently, and
# what covers the pilot variable nobody has invented yet.
#
# Usage:  ./scripts/no-pilot-env.sh cargo test --workspace
set -euo pipefail

# Projection of cosmon_core::pilot_env::PilotVar::ALL — keep in that order.
PILOT_VARS=(
    CLAUDE_CONFIG_DIR
    ANTHROPIC_MODEL
    IS_SANDBOX
    CB_SESSION_ROLE
    CB_DEPTH
    COSMON_MOL_DIR
    COSMON_PARENT_MOL_ID
    COSMON_EGRESS_POLICY
    COSMON_EGRESS_REQUIRE_NETNS
    COSMON_API_REQUEST
    COSMON_ARTIFACT_DIR
)

if [[ $# -eq 0 ]]; then
    printf 'usage: %s <command> [args…]\n' "$0" >&2
    printf 'strips: %s\n' "${PILOT_VARS[*]}" >&2
    exit 2
fi

# `--list` prints the projection and exits, so the parity test reads the list
# from the script that actually applies it rather than re-parsing its source.
if [[ "$1" == "--list" ]]; then
    printf '%s\n' "${PILOT_VARS[@]}"
    exit 0
fi

# macOS ships bash 3.2 — no `mapfile`, no `${var@Q}`. Build the `env -u` argv
# by hand so this behaves identically under the post_merge hook's shell.
unset_args=()
for var in "${PILOT_VARS[@]}"; do
    unset_args+=(-u "$var")
done

# `exec` on purpose: the gate's exit status is this script's exit status, with
# no shell left in between to swallow a signal. A boundary that changed a
# gate's verdict would be the very defect it exists to prevent.
exec env "${unset_args[@]}" "$@"
