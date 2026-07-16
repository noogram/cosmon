# Cosmon — task recipes
# Run `just --list` to see available recipes.

# --- Build & Install ----------------------------------------------------------

# Build & install the public cosmon product binaries to ~/.local/bin.
#
# This is the PUBLIC-SAFE install: it builds only cosmon's own product
# binaries (`cs`, `cs-api`, `cosmon-remote`, `cosmon-daemon-supervisor`)
# plus the `cs` man page. It does NOT touch the private federation tooling
# (`neurion`, `topon-mcp`, `almanac`) — those are an operator-workstation
# concern, installed separately via `just install-federation`.
#
# This recipe is also the `cs done` post-merge hook (config.toml
# [hooks].post_merge): a contributor's `cs done` refreshes only the `cs`
# binary lineage, never the private federation. See ADR-082 / the
# contribution path (task-20260616-0e75) for why the loop is decoupled.
#
# Note: cosmon-mcp is deprecated (see crates/cosmon-mcp/README.md) and is no
# longer installed by this recipe. Workers and the pilot use the `cs` CLI
# exclusively. If a grace-window consumer still needs the standalone binary:
#   cargo build -p cosmon-mcp --release && install target/release/cosmon-mcp ~/.local/bin/cosmon-mcp
install:
    cargo build --release --locked -p cosmon-cli -p cosmon-api -p cosmon-remote -p cosmon-daemon-supervisor
    install target/release/cs ~/.local/bin/cs
    install target/release/cs-api ~/.local/bin/cs-api
    install target/release/cosmon-remote ~/.local/bin/cosmon-remote
    install target/release/cosmon-daemon-supervisor ~/.local/bin/cosmon-daemon-supervisor
    mkdir -p ~/.local/share/man/man1
    install -m 644 crates/cosmon-cli/man/cs.1 ~/.local/share/man/man1/cs.1
    @just _check-cs-multiplicity

# Build & install the PRIVATE federation tooling to ~/.local/bin.
#
# `neurion` (nervous-system registry), `topon-mcp` (codebase topology maps),
# and `almanac` (Zotero/reference layer) are internal Noogram federation
# tools, not part of the public cosmon product. They are vendored in-tree as
# workspace members but are deliberately kept OUT of `just install` so the
# public contribution loop never installs them (see task-20260616-0e75).
#
# Operator-workstation only. Run this on a machine that participates in the
# federation; a public contributor never needs it.
install-federation:
    cargo build --release --locked -p neurion-mcp -p topon-mcp -p almanac
    install target/release/neurion ~/.local/bin/neurion
    install target/release/topon-mcp ~/.local/bin/topon-mcp
    install target/release/almanac ~/.local/bin/almanac

# Operator convenience: full federated-workstation install (product + federation).
# Equivalent to the historical `just install` behaviour before the
# contribution-path split (task-20260616-0e75).
install-all: install install-federation

# Deploy-hygiene self-check (task-20260607-3ad4): `just install` refreshes only
# ~/.local/bin/cs, but stale copies elsewhere on PATH (~/.cargo/bin from a
# `cargo install`, /opt/homebrew/bin from a Cellar formula) drift silently and
# can shadow the fresh build. Warn loudly when more than one distinct `cs` is on
# PATH — never auto-rm (removing a binary is an operator gesture).
_check-cs-multiplicity:
    #!/usr/bin/env bash
    set -euo pipefail
    # macOS ships bash 3.2 — no `mapfile`, no `declare -A`. Stay portable so
    # this guard runs identically under the post_merge hook's restricted PATH.
    distinct=()
    while IFS= read -r h; do
        [ -z "$h" ] && continue
        canon="$(readlink -f "$h" 2>/dev/null || echo "$h")"
        dup=0
        for d in "${distinct[@]:-}"; do
            [ "$d" = "$canon" ] && dup=1 && break
        done
        [ "$dup" -eq 0 ] && distinct+=("$canon")
    done < <(which -a cs 2>/dev/null || true)
    if (( ${#distinct[@]} > 1 )); then
        echo "" >&2
        echo "⚠️  ${#distinct[@]} distinct \`cs\` binaries on PATH — install refreshed only ~/.local/bin/cs; the rest drift silently:" >&2
        i=1
        for d in "${distinct[@]}"; do
            mt="$(stat -f '%Sm' -t '%Y-%m-%d %H:%M' "$d" 2>/dev/null || stat -c '%y' "$d" 2>/dev/null | cut -d. -f1 || echo unknown)"
            echo "    [$i] $d (mtime $mt)" >&2
            i=$((i+1))
        done
        echo "    Stale copies fall to their built-in adapter floor — remove the phantoms manually (rm is operator-gestured)." >&2
        echo "" >&2
    fi

# Build debug binary
build:
    cargo build --workspace --locked

# Cross-compile cosmon-remote for the 4 dist targets the adapter serves
# under `GET /dist/binary/<platform>/cosmon-remote` (task-20260522-aad5).
#
# Targets:
#   macOS-arm64   → aarch64-apple-darwin     (native cargo build)
#   macOS-amd64   → x86_64-apple-darwin      (native cargo build)
#   linux-arm64   → aarch64-unknown-linux-musl (cargo-zigbuild, static)
#   linux-amd64   → x86_64-unknown-linux-musl  (cargo-zigbuild, static)
#
# Output: crates/cosmon-rpp-adapter/assets/binaries/<platform>/cosmon-remote
#
# The adapter's Dockerfile COPYs this directory into the runtime image so
# `GET /dist/binary/<platform>/cosmon-remote` streams the embedded binary
# without rebuilding cosmon-remote inside the container.
#
# Requires: rustup targets installed + cargo-zigbuild + zig 0.16+ for musl.
dist-binaries:
    #!/usr/bin/env bash
    set -euo pipefail
    out="crates/cosmon-rpp-adapter/assets/binaries"
    rm -rf "$out"
    mkdir -p "$out/macos-arm64" "$out/macos-amd64" "$out/linux-arm64" "$out/linux-amd64"

    echo "▸ macOS arm64 (aarch64-apple-darwin)"
    cargo build --release -p cosmon-remote --target aarch64-apple-darwin --locked
    install -m 0755 target/aarch64-apple-darwin/release/cosmon-remote \
        "$out/macos-arm64/cosmon-remote"

    echo "▸ macOS amd64 (x86_64-apple-darwin)"
    cargo build --release -p cosmon-remote --target x86_64-apple-darwin --locked
    install -m 0755 target/x86_64-apple-darwin/release/cosmon-remote \
        "$out/macos-amd64/cosmon-remote"

    echo "▸ Linux arm64 musl (aarch64-unknown-linux-musl, static)"
    cargo zigbuild --release -p cosmon-remote --target aarch64-unknown-linux-musl --locked
    install -m 0755 target/aarch64-unknown-linux-musl/release/cosmon-remote \
        "$out/linux-arm64/cosmon-remote"

    echo "▸ Linux amd64 musl (x86_64-unknown-linux-musl, static)"
    cargo zigbuild --release -p cosmon-remote --target x86_64-unknown-linux-musl --locked
    install -m 0755 target/x86_64-unknown-linux-musl/release/cosmon-remote \
        "$out/linux-amd64/cosmon-remote"

    echo "▸ summary"
    ls -lh "$out"/*/cosmon-remote

# Requires Xcode signing wired for the mac-pilot target — see
# docs/guides/mac-pilot-signing-setup.md. Ad-hoc fallback:
# scripts/mac-pilot-reinstall-adhoc.sh.

# Build + install mac-pilot.app to ~/Applications (team-signed Release build)
install-mac-pilot:
    xcodebuild -project apps/mac-pilot/mac-pilot.xcodeproj \
      -scheme mac-pilot -configuration Release \
      -destination 'platform=macOS,arch=arm64' \
      -derivedDataPath /tmp/mac-pilot-build \
      -allowProvisioningUpdates \
      -quiet \
      build
    pkill -f '/Applications/mac-pilot.app/Contents/MacOS/mac-pilot' || true
    rm -rf ~/Applications/mac-pilot.app
    cp -R /tmp/mac-pilot-build/Build/Products/Release/mac-pilot.app ~/Applications/
    open ~/Applications/mac-pilot.app

# Run all quality gates (check + test + clippy + fmt)
check:
    cargo check --workspace --locked
    cargo test --workspace --locked
    cargo clippy --workspace --locked -- -D warnings
    cargo fmt --all -- --check

# Supply-chain checks (mirror of .github/workflows/deny.yml)
audit:
    cargo deny check
    cargo audit --deny warnings

# cosmon-without-neurion gate — mandatory blocking check.
#
# Runs the substrate tests with PATH stripped of any directory that
# could resolve the `neurion` binary. If the test suite still passes,
# cosmon is provably independent of neurion at cold boot. This is the
# cultural enforcement against Universe D paradigm drift (per
# delib-20260418-1f29 outcomes.md Child C and
# docs/architectural-invariants.md §7c).
#
# Two tests are in scope today:
#  * bootstrap_monotonicity — no `$(neurion …)` substrings in templates
#    that install to ~/Library/LaunchAgents/com.cosmon.* or
#    ~/.config/cosmon/.
#  * restart_fidelity_no_neurion — sibling task-20260418-fb87 (the test
#    file is added by that task). This recipe already wires it in
#    behind a `--ignored`-tolerant invocation so the gate lights up the
#    moment fb87 lands.
#
# Mirror of .github/workflows/ci.yml job `cosmon-without-neurion`.
test-without-neurion:
    #!/usr/bin/env bash
    set -euo pipefail
    # Remove every PATH entry that currently resolves `neurion`. We do
    # this deterministically rather than blanking PATH so that `cargo`,
    # `git`, and the system toolchain remain reachable. We scan PATH
    # ourselves (portable bash, no `command -v -a`, no `which -a`).
    stripped=""
    IFS=':' read -ra pathdirs <<<"$PATH"
    for d in "${pathdirs[@]}"; do
        [ -z "$d" ] && continue
        if [ -x "$d/neurion" ]; then
            continue
        fi
        if [ -z "$stripped" ]; then stripped="$d"; else stripped="$stripped:$d"; fi
    done
    export PATH="$stripped"
    # Sanity: neurion must not be reachable during the test run.
    if command -v neurion >/dev/null 2>&1; then
        echo "test-without-neurion: failed to strip neurion from PATH — aborting" >&2
        exit 1
    fi
    # Bootstrap-monotonicity grep test — always runs.
    cargo test -p cosmon-cli --test bootstrap_monotonicity --locked
    # Restart-fidelity test — runs only if sibling task-20260418-fb87
    # has landed its test file. Until then, the gate is the grep test
    # alone.
    if [ -f crates/cosmon-cli/tests/restart_fidelity_no_neurion.rs ]; then
        cargo test -p cosmon-cli --test restart_fidelity_no_neurion --locked
    else
        echo "test-without-neurion: restart_fidelity_no_neurion.rs not present yet (task-20260418-fb87) — skipped"
    fi

# Archive integrity check — verify every archive entry modified in the
# last 7 days. Mirror of .github/workflows/archive-verify.yml so the
# gate behaves identically on laptop and CI. Uses the debug `cs` build
# so it runs against the worktree's current state; swap to release if
# you want post-install behaviour.
archive-verify SINCE_DAYS="7":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --bin cs -p cosmon-cli --locked
    ids=$(./target/debug/cs archive list --since-days {{SINCE_DAYS}})
    if [ -z "$ids" ]; then
        echo "archive-verify: no archive entries in the last {{SINCE_DAYS}} day(s) — nothing to check"
        exit 0
    fi
    failed=0
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        echo "archive-verify: $id"
        if ! ./target/debug/cs archive verify "$id"; then
            failed=$((failed + 1))
        fi
    done <<< "$ids"
    if [ "$failed" -ne 0 ]; then
        echo "archive-verify: $failed entries failed"
        exit 1
    fi
    echo "archive-verify: all entries pass"

# --- Resident Runtime (ADR-095) -----------------------------------------------

# Run the ADR-095 Resident Runtime in the foreground, tailing its NDJSON
# trace. Ctrl-C tears down both the runtime and the tail. Use this to
# observe live dispatch decisions during self-application.
#
# The runtime walks the whole ensemble, so no positional argument is
# needed. `--poll-interval 2` matches the launchd template.
#
# The trace file lives at `.cosmon/state/runtime-trace.jsonl` and accrues
# one NDJSON line per loop iteration. Per ADR-095 §2 RR-5, each line
# carries: `ts`, `action`, `decision_basis`, `molecule_id`,
# `invocation_uuid`, `state_hash_before`, `state_hash_after`, `error`.
self-runtime:
    #!/usr/bin/env bash
    set -uo pipefail
    trace=".cosmon/state/runtime-trace.jsonl"
    mkdir -p "$(dirname "$trace")"
    : >> "$trace"            # touch so tail -f doesn't race the writer
    cargo build --bin cs -p cosmon-cli --locked
    echo "→ self-runtime: trace at $trace"
    echo "→ Ctrl-C stops both the runtime and the trace tail."
    tail -F "$trace" &
    tail_pid=$!
    trap 'kill $tail_pid 2>/dev/null || true' EXIT INT TERM
    ./target/debug/cs run --resident --poll-interval 2

# --- TLA+ TLC validation ------------------------------------------------------
# Validate CosmonRun.tla + CosmonRunScheduler.tla against the 8 checked configs.
#   CosmonRun_InBand                 → No error
#   CosmonRun_OutOfBand              → I9 violated (Gödel sentence via BypassMerge)
#   CosmonRun_Crashes                → No error (I5 liveness rattrape)
#   CosmonRun_CrashesI3/I4           → I3/I4 violated (eventual safety — prose amendment pending)
#   CosmonRun_I9Counterexample       → I9 violated (explicit demonstration)
#   CosmonRunScheduler_Normal        → No error (S1/S2/S3 hold + L2 liveness)
#   CosmonRunScheduler_ConvoyCascade → S3 violated (convoy cascade counterexample)
# Zero warnings expected on all. See docs/specs/VALIDATION-REPORT.md for the
# full rationale. Requires Java 17+ (brew install openjdk@17).

JAVA := env_var_or_default("JAVA", "/opt/homebrew/opt/openjdk@17/bin/java")

# Run TLC on every CosmonRun_*.cfg config. Prints pass/fail per config.
tla-verify:
    #!/usr/bin/env bash
    set -uo pipefail
    cd docs/specs
    test -x {{JAVA}} || { echo "ERROR: java not found at {{JAVA}} — try 'brew install openjdk@17' or export JAVA=/your/java"; exit 2; }
    fail=0
    for cfg in CosmonRun_*.cfg CosmonRunScheduler_*.cfg; do
        expected_violation=""
        case "$cfg" in
            CosmonRun_OutOfBand.cfg)              expected_violation="I9_BranchMergedOnlyIfCompleted" ;;
            CosmonRun_CrashesI3.cfg)              expected_violation="I3_FleetMirrorsSession" ;;
            CosmonRun_CrashesI4.cfg)              expected_violation="I4_SessionImpliesLiveProcess" ;;
            CosmonRun_I9Counterexample.cfg)       expected_violation="I9_BranchMergedOnlyIfCompleted" ;;
            CosmonRunScheduler_ConvoyCascade.cfg) expected_violation="S3_PurgeBeforeRespawn" ;;
        esac
        case "$cfg" in
            CosmonRunScheduler_*.cfg) module="CosmonRunScheduler.tla" ;;
            *)                        module="CosmonRun.tla" ;;
        esac
        out=$({{JAVA}} -XX:+UseParallelGC -jar tla2tools.jar -config "$cfg" "$module" 2>&1)
        n_warn=$(echo "$out" | grep -c -iE "^Warning:|^Warn:")
        verdict=$(echo "$out" | grep -E "Model checking completed|Invariant.*violated" | head -1)
        if [ -n "$expected_violation" ]; then
            if echo "$out" | grep -q "Invariant $expected_violation is violated"; then
                printf "%-40s | warnings=%s | ✅ expected violation: %s\n" "$cfg" "$n_warn" "$expected_violation"
            else
                printf "%-40s | warnings=%s | ❌ MISSING expected violation of %s\n" "$cfg" "$n_warn" "$expected_violation"
                fail=$((fail+1))
            fi
        else
            if echo "$out" | grep -q "Model checking completed. No error has been found"; then
                printf "%-40s | warnings=%s | ✅ clean\n" "$cfg" "$n_warn"
            else
                printf "%-40s | warnings=%s | ❌ UNEXPECTED: %s\n" "$cfg" "$n_warn" "$verdict"
                fail=$((fail+1))
            fi
        fi
    done
    if [ "$fail" -ne 0 ]; then
        echo ""
        echo "tla-verify: $fail config(s) diverged from expected outcome"
        exit 1
    fi
    echo ""
    echo "tla-verify: all 9 configs match expected outcome (CosmonRun I3..I9 + I_StepProgress + CosmonRunScheduler S1..S3 schedulers)"

# Run a single TLC config (verbose output to stdout). Dispatches the
# spec module by config-name prefix.
tla-one CONFIG:
    #!/usr/bin/env bash
    set -uo pipefail
    cd docs/specs
    case "{{CONFIG}}" in
        CosmonRunScheduler_*.cfg)   module="CosmonRunScheduler.tla" ;;
        CosmonDocHarness*.cfg)      module="CosmonDocHarness.tla" ;;
        *)                          module="CosmonRun.tla" ;;
    esac
    {{JAVA}} -XX:+UseParallelGC -jar tla2tools.jar -config {{CONFIG}} "$module"

# --- DOC-HARNESS meta-fleet (CosmonDocHarness.tla) ---------------------------
# Runs both CosmonDocHarness.cfg (tight: liveness + safety) and
# CosmonDocHarness_Safety.cfg (widened: safety only) and seals the result
# in `docs/specs/CosmonDocHarness.tlc-green` (BLAKE3(.tla) + UTC timestamp).
#
# Two-step contract per torvalds §4 (delib-20260519-a20b):
#   - both configs green -> write the sentinel ;
#   - any non-zero exit  -> delete the sentinel and exit non-zero.
#
# Downstream readers (`cs config adapters`, `man cs LOOPS / ADAPTERS`) gate
# their own outputs on the presence of this sentinel.
tlc:
    #!/usr/bin/env bash
    set -uo pipefail
    cd docs/specs
    sentinel="CosmonDocHarness.tlc-green"
    spec="CosmonDocHarness.tla"
    tight="CosmonDocHarness.cfg"
    widened="CosmonDocHarness_Safety.cfg"

    test -x {{JAVA}} || { echo "ERROR: java not found at {{JAVA}}"; rm -f "$sentinel"; exit 2; }
    command -v b3sum >/dev/null 2>&1 || { echo "ERROR: b3sum not found in PATH (cargo install b3sum)"; rm -f "$sentinel"; exit 2; }

    fail=0
    for cfg in "$tight" "$widened"; do
        echo "tlc: running $cfg against $spec ..."
        out=$({{JAVA}} -XX:+UseParallelGC -jar tla2tools.jar \
                -workers auto -config "$cfg" "$spec" 2>&1)
        if echo "$out" | grep -q "Model checking completed. No error has been found"; then
            printf "  %-38s | OK\n" "$cfg"
        else
            verdict=$(echo "$out" | grep -E "Model checking completed|Invariant.*violated|Error" | head -1)
            printf "  %-38s | FAIL: %s\n" "$cfg" "$verdict"
            fail=$((fail+1))
        fi
    done

    if [ "$fail" -ne 0 ]; then
        echo "tlc: $fail config(s) failed — deleting sentinel"
        rm -f "$sentinel"
        exit 1
    fi

    hash=$(b3sum --no-names "$spec")
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    {
        echo "spec=$spec"
        echo "blake3=$hash"
        echo "verified_at=$ts"
    } > "$sentinel"
    echo "tlc: both configs green ; sealed $sentinel"

# --- Paper compilation --------------------------------------------------------

# Build the paper PDF (latexmk -pdf)
paper-build:
    latexmk -pdf -output-directory=docs/paper docs/paper/cosmon-paper.tex

# Clean paper build artifacts
paper-clean:
    latexmk -C -output-directory=docs/paper docs/paper/cosmon-paper.tex

# Watch and continuously recompile paper on changes
paper-watch:
    latexmk -pvc -pdf -output-directory=docs/paper docs/paper/cosmon-paper.tex
