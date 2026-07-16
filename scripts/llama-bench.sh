#!/usr/bin/env bash
# scripts/llama-bench.sh — cross-validate the Rust bench harness against
# upstream llama.cpp's `llama-bench` tool.
#
# The Rust harness in crates/cosmon-provider/benches/llama_bench.rs
# measures TTFT and total wall-clock through the cosmon-llama wrapper.
# This script runs the upstream tool on the **same** GGUFs so the two
# decode-tok/s columns can be compared. Acceptance criterion (briefing):
# the two numbers agree within ±10 %.
#
# The vendored llama.cpp snapshot at crates/cosmon-llama-sys/vendor/
# llama.cpp/ ships only library sources, no examples. `llama-bench` is
# expected on PATH; install it via:
#
#   brew install llama.cpp     # ships `llama-bench` on macOS
#   # or build from a separate llama.cpp checkout: cmake --build build --target llama-bench
#
# Env vars (same as the Rust harness — pass the same paths to both):
#   COSMON_LLAMA_BENCH_GGUF_8B          → Llama-3.2-8B-Instruct  Q8_0
#   COSMON_LLAMA_BENCH_GGUF_24B         → Mistral-Small-24B      Q5_K_M
#   COSMON_LLAMA_BENCH_GGUF_32B         → Qwen2.5-32B            Q5_K_M
#   COSMON_LLAMA_BENCH_GGUF_CODER_32B   → Qwen2.5-Coder-32B      Q4_K_M
#   COSMON_LLAMA_BENCH_GGUF_70B         → Llama-3.3-70B          Q4_K_M
#
# Flags forwarded to `llama-bench`:
#   -p 100,1500,4096   prompt sizes (haiku / small-code-edit / reasoning-chain)
#   -n 200             output token cap
#   -ngl 99            offload all layers to Metal (no CPU fallback)
#   -r 1               one repetition per cell — these models are slow enough
#                      that a single shot is fine and matches the Rust harness.
#
# Output:
#   stdout — Markdown table from `llama-bench --output md`, suitable
#            for pasting into docs/benches/llama-v0-acceptance.md.
#   stderr — progress and fixture-discovery messages.
#
# Exit code: 0 if every present fixture ran to completion; 1 if any
# llama-bench invocation failed; 2 if `llama-bench` is not on PATH.

set -euo pipefail

if ! command -v llama-bench >/dev/null 2>&1; then
    echo "error: llama-bench not on PATH" >&2
    echo "  install via: brew install llama.cpp" >&2
    echo "  (or build from a separate llama.cpp checkout)" >&2
    exit 2
fi

LLAMA_BENCH_BIN="$(command -v llama-bench)"
echo "# using: ${LLAMA_BENCH_BIN}" >&2
"${LLAMA_BENCH_BIN}" --version 2>&1 | sed 's/^/# /' >&2
echo "" >&2

# Common arguments — keep aligned with the Rust harness so the two
# numbers are comparable.
COMMON_ARGS=(-p 100,1500,4096 -n 200 -ngl 99 -r 1 --output md)

# (env_var, display_name, quantisation)
FIXTURES=(
    "COSMON_LLAMA_BENCH_GGUF_8B|Llama-3.2-8B-Instruct|Q8_0"
    "COSMON_LLAMA_BENCH_GGUF_24B|Mistral-Small-24B-Instruct-2501|Q5_K_M"
    "COSMON_LLAMA_BENCH_GGUF_32B|Qwen2.5-32B-Instruct|Q5_K_M"
    "COSMON_LLAMA_BENCH_GGUF_CODER_32B|Qwen2.5-Coder-32B-Instruct|Q4_K_M"
    "COSMON_LLAMA_BENCH_GGUF_70B|Llama-3.3-70B-Instruct|Q4_K_M"
)

had_failure=0
ran_anything=0

for fixture in "${FIXTURES[@]}"; do
    IFS='|' read -r env_var display_name quant <<<"${fixture}"
    path="${!env_var:-}"
    if [[ -z "${path}" ]]; then
        echo "# skip ${display_name} (${quant}) — \$${env_var} unset" >&2
        continue
    fi
    if [[ ! -f "${path}" ]]; then
        echo "# skip ${display_name} (${quant}) — path not found: ${path}" >&2
        continue
    fi
    ran_anything=1
    echo "" >&2
    echo "## ${display_name} — ${quant}" >&2
    echo "" >&2
    if ! "${LLAMA_BENCH_BIN}" -m "${path}" "${COMMON_ARGS[@]}"; then
        echo "# error running ${display_name} (${quant})" >&2
        had_failure=1
    fi
done

if [[ "${ran_anything}" -eq 0 ]]; then
    echo "# no fixture env vars were set — nothing to do." >&2
    echo "# set at least one of COSMON_LLAMA_BENCH_GGUF_{8B,24B,32B,CODER_32B,70B}=<path>" >&2
    exit 2
fi

exit "${had_failure}"
