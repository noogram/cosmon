#!/usr/bin/env bash
# forbid-matrix-features.sh — CI lint guarding against matrix-sdk feature
# drift in the cosmon-matrix-tick ingress crate.
#
# The bridge is deliberately configured with the most surgical feature
# set the SDK allows: no E2E, no markdown rendering, no TLS default.
# Forgemaster surfaced this as a red flag against the unpatched
# 2026 vodozemac disclosure (default-features default drift). If any of
# those features turn back on — through `default-features = true`, a
# transitive dependency pulling them in, or a well-intentioned "let's
# enable encryption" PR — this lint fails the build.
#
# The test is `cargo tree -e features` on matrix-sdk and a grep for the
# three forbidden feature names. Running the check requires a resolved
# lock file (cargo-tree does a no-op update otherwise).
#
# Rationale:
#   - delib-20260422-c4a6 Forgemaster §"Red flags" item 1
#   - delib-20260422-c4a6 synthesis §C4 (minimal surface principle)
#
# Bypass: COSMON_SKIP_MATRIX_FEATURE_LINT=1 (logged, returns 0).

set -euo pipefail

if [ "${COSMON_SKIP_MATRIX_FEATURE_LINT:-}" = "1" ]; then
    echo "forbid-matrix-features: bypassed via COSMON_SKIP_MATRIX_FEATURE_LINT=1" >&2
    exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Same ship-before-crate policy as forbid-matrix-send.sh: if matrix-sdk
# is not in the resolved workspace yet, this is a clean pass.
if ! cargo metadata --format-version 1 --no-deps --locked 2>/dev/null \
        | grep -q '"cosmon-matrix-tick"'; then
    echo "forbid-matrix-features: cosmon-matrix-tick not in workspace yet — clean pass." >&2
    exit 0
fi

if ! cargo tree -p matrix-sdk -e features --locked >/dev/null 2>&1; then
    echo "forbid-matrix-features: matrix-sdk not in resolved graph — clean pass." >&2
    exit 0
fi

FORBIDDEN_FEATURES=(
    'e2e-encryption'
    'markdown'
    'native-tls'
)

tree=$(cargo tree -p matrix-sdk -e features --locked 2>&1)

failed=0
matches=()
for feat in "${FORBIDDEN_FEATURES[@]}"; do
    # cargo tree prints features as `"<name>"`; match that exact form so
    # we don't flag a package called `markdown-foo` as a feature.
    if echo "$tree" | grep -qE "\"${feat}\""; then
        matches+=("$feat")
        failed=1
    fi
done

if [ "$failed" -eq 0 ]; then
    echo "forbid-matrix-features: matrix-sdk feature surface is clean ✓"
    exit 0
fi

echo ""
echo "✗ CI LINT FAILED — forbidden matrix-sdk features enabled."
echo ""
for feat in "${matches[@]}"; do
    echo "    Enabled: \"$feat\""
done
echo ""
cat <<'EOF'
    Rationale:
    - delib-20260422-c4a6 Forgemaster §"Red flags" item 1
      (unpatched 2026 vodozemac disclosure, default-features drift)
    - delib-20260422-c4a6 synthesis §C4 (minimal surface principle)

    Fix: in crates/cosmon-matrix-tick/Cargo.toml, set
        matrix-sdk = { version = "...", default-features = false,
                       features = ["sync"] }
    and audit transitive pulls with `cargo tree -p matrix-sdk -e features`.
EOF

exit 1
