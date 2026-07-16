#!/usr/bin/env bash
# Entrypoint baked into the Tenant-Demo strace-test image. Two modes:
#
#   Default (v1)            — static-surface sweep: --version, --help, init, peek,
#                             notarize --help. Thin and representative.
#   FULL_LIFECYCLE=1 (v2)    — end-to-end cosmon cycle on a throwaway fake-target
#                             git repo: init → nucleate → tackle (will fail
#                             gracefully without Claude) → observe → peek → done.
#                             Uses `strace -f -e file` so forked children (tmux,
#                             git, worktree helpers) are captured too.
#
# Output:
#   /out/*.trace              — one per captured command
#   /out/SUMMARY.md           — human-readable index + syscall category counts
#   /out/README.md            — bundle layout note
#   /out/fake-target/         — the throwaway git repo created in full-lifecycle
#                               mode (copied out so auditors can inspect it)
#   /out/binary/cs            — the cosmon binary used to produce the traces
#
# See Dockerfile.strace-test and scripts/docker-strace-test.sh.

set -euo pipefail

OUT=/out
mkdir -p "${OUT}"

MODE="${FULL_LIFECYCLE:-0}"

# Helper — run a command under strace -f -e file and record its exit code.
# A non-zero exit is non-fatal (we still want the trace); "|| true" keeps
# the sweep going so a single flaky command doesn't abort the bundle.
trace_cmd() {
    local out_file="$1"; shift
    echo "==> strace $*"
    strace -f -e file -o "${out_file}" "$@" || true
}

# ---------------------------------------------------------------------------
# Static-surface sweep (always run). These are thin, representative commands
# that exercise the cosmon binary surface without a live state store.
# ---------------------------------------------------------------------------

trace_cmd "${OUT}/cs-version.trace"   cs --version
trace_cmd "${OUT}/cs-help.trace"      cs --help

# Static init in a throwaway galaxy so `cs init` has a git repo to attach to.
mkdir -p /workspace/demo
pushd /workspace/demo >/dev/null
git init --quiet
trace_cmd "${OUT}/cs-init.trace"      cs init --json
popd >/dev/null

trace_cmd "${OUT}/cs-peek-help.trace" cs peek --help
trace_cmd "${OUT}/cs-notarize.trace"  cs notarize --help

# ---------------------------------------------------------------------------
# Full-lifecycle sweep (opt-in via FULL_LIFECYCLE=1). Exercises the code path
# tenant_auditor actually needs to audit: fork + tmux + git worktree + subprocess.
#
# The Claude binary is intentionally NOT installed in the image. `cs tackle`
# will attempt to spawn a worker, fail to find `claude`, and exit — but the
# strace -f capture still records the syscalls leading up to that point,
# which is the representative surface Tenant-Demo's pipeline must protect.
# ---------------------------------------------------------------------------

if [[ "${MODE}" == "1" ]]; then
    echo
    echo "==> Full lifecycle mode — building throwaway fake-target repo"

    FAKE=/workspace/fake-target
    rm -rf "${FAKE}"
    mkdir -p "${FAKE}"
    cd "${FAKE}"
    git init --quiet -b main
    git config user.email "demo@cosmon.test"
    git config user.name  "Demo"
    echo "hello" > README.md
    git add .
    git commit --quiet -m "initial"

    # (d) cs init with full lifecycle (separate trace from the static one so
    #     an auditor can compare a cold init vs. a lifecycle init).
    trace_cmd "${OUT}/cs-init-full.trace"  cs init --json

    # (e) cs nucleate — capture the molecule id from the JSON emitted on stdout.
    echo "==> strace cs nucleate task-work"
    strace -f -e file -o "${OUT}/cs-nucleate.trace" \
        cs nucleate task-work \
            --var topic='fake target demo' \
            --json \
        > /tmp/nucleate.out 2> /tmp/nucleate.err \
        || true

    MOL_ID="$(jq -r '.molecule_id // .id // empty' < /tmp/nucleate.out 2>/dev/null || true)"
    if [[ -z "${MOL_ID}" ]]; then
        # Fallback — parse a `task-...` id from stdout or the state dir.
        MOL_ID="$(grep -Eo 'task-[0-9a-f-]+' /tmp/nucleate.out 2>/dev/null | head -n1 || true)"
    fi
    if [[ -z "${MOL_ID}" ]]; then
        # Final fallback — scan the freshly written state directory.
        MOL_ID="$(find .cosmon/state/molecules -maxdepth 2 -type d -name 'task-*' 2>/dev/null | head -n1 | xargs -I{} basename {} || true)"
    fi
    echo "==> Captured molecule id: ${MOL_ID:-<none>}"
    echo "${MOL_ID:-}" > "${OUT}/fake-target-molecule-id.txt"

    if [[ -n "${MOL_ID}" ]]; then
        # (g) cs tackle — will fork tmux + git worktree. Without a `claude`
        #     binary installed, the worker pane will fail, but strace -f
        #     captures the interesting syscalls up to that point.
        trace_cmd "${OUT}/cs-tackle.trace" \
            cs tackle "${MOL_ID}" --leaf --force-runtime

        # (h) brief settle so the state store picks up the tackle write.
        sleep 2
        trace_cmd "${OUT}/cs-observe.trace" \
            cs observe "${MOL_ID}" --json

        # (i) cs peek in plaintext mode (no TUI). --once keeps it bounded.
        trace_cmd "${OUT}/cs-peek.trace" \
            cs peek --no-tui --once

        # (j) cs done --force — exercise the teardown code path even though
        #     the worker never completed.
        trace_cmd "${OUT}/cs-done.trace" \
            cs done "${MOL_ID}" --force
    else
        echo "==> WARNING: could not capture molecule id; skipping tackle/observe/done"
    fi

    # Copy the fake-target into /out so auditors can inspect the git repo
    # and the .cosmon/ state produced by the cycle. Strip .git objects
    # beyond what is needed to keep the bundle small.
    echo "==> Copying fake-target into /out/fake-target/"
    mkdir -p "${OUT}/fake-target"
    # Keep top-level files + .cosmon/ + a shallow git ref tree.
    cp -a "${FAKE}/." "${OUT}/fake-target/" 2>/dev/null || true
    # Drop heavy git blobs — keep HEAD and refs as structural evidence only.
    rm -rf "${OUT}/fake-target/.git/objects" 2>/dev/null || true
    # Drop any worktree checkouts (large, redundant).
    rm -rf "${OUT}/fake-target/.worktrees" 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# Bundle metadata — SUMMARY.md + README.md with syscall category counts.
# ---------------------------------------------------------------------------

# The cosmon binary itself is intentionally NOT copied into the bundle:
# it weighs ~18 MB (un-stripped Rust debug info stripped notwithstanding)
# and pushes the tarball above the 100 KB budget we give Tenant-Demo. Build
# the binary locally via `bash scripts/docker-strace-test.sh [--rebuild]`
# (see Dockerfile.strace-test) to reproduce it byte-for-byte from the
# pinned toolchain. We still record its size and version below so an
# auditor knows what produced the traces.
BIN_SIZE="$(stat -c%s /usr/local/bin/cs 2>/dev/null || echo '?')"
BIN_VERSION="$(cs --version 2>/dev/null || echo 'cs --version failed')"
KERNEL="$(uname -a)"
ARCH="$(uname -m)"

# Bash helper — count matching lines in a trace file (0 if absent).
# grep -c always prints exactly one integer on stdout (including 0 for
# "no match"); we just need to suppress the exit-1 that grep returns
# when nothing matches, so `|| true` — NOT `|| echo 0` (that would emit
# a second line and break printf downstream).
count_in() {
    local pat="$1"
    local file="$2"
    if [[ -f "${file}" ]]; then
        grep -cE "${pat}" "${file}" 2>/dev/null || true
    else
        echo 0
    fi
}

{
    echo "# Tenant-Demo strace compatibility test — summary"
    echo
    echo "Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "Binary:    ${BIN_VERSION}"
    echo "Binary sz: ${BIN_SIZE} bytes"
    echo "Kernel:    ${KERNEL}"
    echo "Arch:      ${ARCH}"
    echo "Mode:      $([[ "${MODE}" == "1" ]] && echo 'full-lifecycle (v2)' || echo 'static-surface (v1)')"
    if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
        echo "API key:   absent — cs tackle will not reach inference; syscall"
        echo "           surface up to spawn is still captured via strace -f."
    else
        echo "API key:   present (value redacted)"
    fi
    echo
    echo "## Traces"
    echo
    printf '  %-28s %8s   %8s   %5s  %5s  %5s  %5s\n' \
        "file" "bytes" "lines" "open" "stat" "exec" "unlnk"
    for f in "${OUT}"/*.trace; do
        [[ -f "${f}" ]] || continue
        lines="$(wc -l <"${f}" | tr -d ' ')"
        size="$(stat -c%s "${f}")"
        opens="$(count_in '(openat|open)\(' "${f}")"
        stats="$(count_in '(stat|fstat|newfstatat|lstat)\(' "${f}")"
        execs="$(count_in 'execve\(' "${f}")"
        unlinks="$(count_in '(unlink|unlinkat|rmdir|renameat|rename)\(' "${f}")"
        printf '  %-28s %8d   %8d   %5d  %5d  %5d  %5d\n' \
            "$(basename "${f}")" "${size:-0}" "${lines:-0}" \
            "${opens:-0}" "${stats:-0}" "${execs:-0}" "${unlinks:-0}"
    done
    echo
    echo "Columns chosen to reflect the \`-e file\` syscall set only:"
    echo "  open  = openat + open"
    echo "  stat  = stat + fstat + newfstatat + lstat"
    echo "  exec  = execve (file-category; loads a binary into the address space)"
    echo "  unlnk = unlink + unlinkat + rmdir + rename + renameat (mutation)"
    echo
    echo "Process-category syscalls (clone/fork/vfork) and memory syscalls"
    echo "(mmap/mprotect) are deliberately filtered out by \`-e file\` to keep"
    echo "the bundle light. Forked children are still captured thanks to"
    echo "\`strace -f\` — they just appear as sub-PID file syscalls, not as"
    echo "clone() calls themselves."
    echo
    if [[ "${MODE}" == "1" ]]; then
        echo "## Lifecycle cycle mapping"
        echo
        echo "  cs-init-full.trace   — fake-target/cs-init (cold repo + .cosmon/)"
        echo "  cs-nucleate.trace    — cs nucleate task-work --var topic=..."
        echo "  cs-tackle.trace      — cs tackle <mol> --leaf --force-runtime"
        echo "                         (forks tmux + git worktree; claude absent)"
        echo "  cs-observe.trace     — cs observe <mol> --json"
        echo "  cs-peek.trace        — cs peek --no-tui --once"
        echo "  cs-done.trace        — cs done <mol> --force (teardown path)"
    fi
} > "${OUT}/SUMMARY.md"

# Micro README for the bundle recipient.
cat > "${OUT}/README.md" <<'EOF'
# cosmon strace bundle — layout

- `SUMMARY.md` — generated index of traces with syscall category counts.
- `*.trace` — `strace -f -e file` output, one per captured `cs` command.
- `fake-target/` *(full-lifecycle only)* — the throwaway git repo + `.cosmon/`
  state produced by the end-to-end cycle (`.git/objects` stripped to keep
  the bundle small; enough structural evidence remains for an audit).
- `fake-target-molecule-id.txt` *(full-lifecycle only)* — the molecule id
  assigned to the fake-target cycle.

Static-surface mode (v1) sweeps: `--version`, `--help`, `init`, `peek --help`,
`notarize --help`. Full-lifecycle mode (v2) additionally sweeps the complete
`nucleate → tackle → observe → peek → done` cycle on a fake-target git repo,
with `strace -f` so forked children (tmux, git worktree helpers) are captured.

The Claude binary is intentionally NOT installed in the image. `cs tackle`
will spawn a tmux pane and fail to launch a Claude worker; `strace -f`
still captures the syscalls leading up to that point, which is the
representative surface for Tenant-Demo's code-protection pipeline.

## Reproducing the binary

The `cs` binary itself is not shipped in this bundle (it weighs ~18 MB,
dominating the tarball). Build it locally from the cosmon repo:

    bash scripts/docker-strace-test.sh --full-lifecycle --rebuild

`Dockerfile.strace-test` pins the Rust toolchain and produces a
byte-for-byte identical binary; `/usr/local/bin/cs` inside the image
is the exact artifact that produced these traces.
EOF

echo
echo "==> Traces generated:"
ls -la "${OUT}"/*.trace "${OUT}/SUMMARY.md" "${OUT}/README.md" 2>/dev/null || true
