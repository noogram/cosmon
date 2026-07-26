#!/usr/bin/env bash
# in-container-test.sh — entrypoint for the netns egress-guard test
# (cs-pilot increment 2, TEST B — task-20260601-070b). Runs INSIDE the
# ephemeral Linux container and drives the pre-compiled
# `exec_command_netns_e2e` integration test with the COSMON_NETNS_E2E
# gate set, so the test's real assertions (which only make sense on a
# netns-capable kernel) actually run.
#
# Exits non-zero on the first failure so the host driver (and CI) can
# gate on it.
set -euo pipefail

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()  { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# 0. Structural preconditions ----------------------------------------
[ "$(uname -s)" = "Linux" ] || die "not Linux — this test requires a netns-capable kernel"
command -v unshare >/dev/null 2>&1 || die "unshare (util-linux) not found — cannot create a netns"

# Probe that unprivileged user+net namespaces actually work HERE. Several
# independent layers can refuse it — a restrictive sysctl, a seccomp filter
# (Docker's default profile blocks `unshare` on some engines), an AppArmor or
# SELinux policy. So attempt the operation, report the REAL refusal text, and
# name none of those as the cause: guessing one is what task-20260726-eabf
# had to undo. Fail here rather than letting the test report a confusing
# spawn error later.
say "Probing unprivileged user+net namespace support ..."
if ! unshare --user --map-root-user --net -- true 2>/tmp/netns-probe.err; then
  die "'unshare --user --map-root-user --net' is refused here: $(cat /tmp/netns-probe.err) \
— the refusal can come from a sysctl, a seccomp profile, AppArmor or SELinux; this probe does \
not know which. Check the sysctls (kernel.unprivileged_userns_clone, user.max_user_namespaces, \
kernel.apparmor_restrict_unprivileged_userns), 'grep Seccomp /proc/self/status', and the \
engine's security options. The colima default kernel with the default profile passes this probe."
fi
ok "kernel supports an unprivileged egress-denied network namespace"

# 1. Run the real production-path test with the gate set -------------
# The test is pre-compiled in the image (cargo test --no-run), so this
# re-run is offline and fast. --include-ignored is harmless (no ignored
# tests) and future-proofs the invocation.
say "Running exec_command_netns_e2e under COSMON_NETNS_E2E=1 ..."
cd /build
COSMON_NETNS_E2E=1 \
  cargo test -p cosmon-agent-harness --test exec_command_netns_e2e \
  -- --nocapture --include-ignored

printf '\n\033[1;32m═══ NETNS EGRESS GUARD GREEN: deny-external is KERNEL-ENFORCED on Linux ═══\033[0m\n'
printf 'Baseline reached 1.1.1.1:443 (container has egress); under\n'
printf 'COSMON_EGRESS_POLICY=deny-external the SAME probe was unreachable\n'
printf '(no route in the netns), while a local `echo` still ran. The\n'
printf 'macOS-only Advisory gap (task-20260530-d8bc) is closed by construction.\n'
