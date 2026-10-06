# WSL2 host validation

This runbook produces a bounded, resumable witness for the Linux user-service
release candidate. It does not turn candidate evidence into a release claim. A
second run against the served installer and the exact published archives is
required after release.

The lane uses an ordinary, disposable test account on Ubuntu 24.04 under WSL2.
The account must have systemd enabled, lingering enabled, its home on the Linux
filesystem, an existing Git identity, and a configured execution adapter. The
test never changes Git identity or linger policy. It leaves the candidate
services installed and running for the power-boundary phases.

The identity may be global or local to an existing repository. If it is
repository-local, set `COSMON_WSL2_IDENTITY_REPO` to that repository. The
witness carries the existing values in the lifecycle process environment; it
does not write Git configuration.

## Roles and evidence

The build operator creates the candidate archives from the revision under
test. The distribution operator runs the phases that execute inside WSL2. The
external driver alone closes the last shell, shuts down or relaunches the
distribution, reboots the host, and sleeps or wakes the host. The validation
script never initiates one of those boundaries from inside the distribution.

Every phase appends a log and writes a checkpoint under
`~/.cosmon/wsl2-host-validation/runs/<run>/`. Re-running a completed read-only
phase is safe. Mutating phases refuse ambiguous reuse, notably the lifecycle
phase's existing disposable repository. Use a new run id for a clean rerun.

Preserve the complete run directory outside the disposable distribution before
teardown. Only sanitized measurements belong in the repository: no host name,
user name, address, account identity, or private network name.

## Build the candidate

Use the pinned toolchain and build the binaries named by
`packaging/shipped-binaries.txt` for `x86_64-unknown-linux-musl`. Package them
with the same two archive names and member layout as the release workflow:

- `cosmon-<version>-x86_64-unknown-linux-musl.tar.gz` contains `cs` and
  `cosmon-remote`.
- `cosmon-service-<version>-x86_64-unknown-linux-musl.tar.gz` contains the four
  service-side binaries plus `scripts/` with both installers, their shared
  helper, and the native templates.

Put both archive checksums in `SHA256SUMS`. Add the exact source revision in a
separate `REVISION` file and copy `infra/install/install.sh` as `install.sh`.
Record the hashes before transferring the directory to the test account. This
is a candidate mirror, not a served release.

## Run the inside-distribution phases

Set one stable run id and the copied candidate directory for every invocation:

```sh
export COSMON_WSL2_RUN_ID=candidate-<short-revision>
export COSMON_CANDIDATE_DIR="$HOME/cosmon-candidate"
export COSMON_WSL2_ADAPTER=<configured-adapter>
export COSMON_WSL2_MODEL=<approved-small-model>
# Only when the identity is repository-local:
export COSMON_WSL2_IDENTITY_REPO="$HOME/<existing-repository>"
verify="$COSMON_CANDIDATE_DIR/verify-wsl2-host.sh"
```

Run these phases in order:

```sh
bash "$verify" preflight
bash "$verify" published-red
bash "$verify" candidate-install
bash "$verify" service-baseline
bash "$verify" lifecycle
bash "$verify" supervisor-crash
bash "$verify" child-crash
bash "$verify" timer
```

`published-red` uses the v0.7.2 public installer and requires the service
installation request to fail before it writes units. `candidate-install` uses
the same public installer code against the local candidate mirror. The
lifecycle phase is the only paid execution task in this procedure: it creates
one file, commits it, completes its molecule, and is then harvested from the
repository root outside the worker. The phase verifies merge ancestry, artifact
content, worktree removal, and the absence of a registered worker worktree.

## External-driver phases

Each `before-*` phase checks the services, writes a checkpoint, and prints the
external action plus the exact resume phase. Do not run a power action in the
same shell as the validation process.

Last-shell logout:

```sh
bash "$verify" before-logout
# External driver closes every shell, waits 30 seconds, then reconnects.
bash "$verify" after-logout
```

Distribution shutdown and explicit relaunch:

```sh
bash "$verify" before-distribution
# External driver shuts down this distribution and explicitly relaunches it.
bash "$verify" after-distribution
```

Host reboot and explicit distribution launch:

```sh
bash "$verify" before-reboot
# External driver uses Restart, not Shut down, and explicitly launches the
# distribution. Shut down with Fast Startup can retain the old host boot time.
bash "$verify" after-reboot
```

The script reads the host boot epoch through `powershell.exe` interop. When
interop is unavailable, the external driver must pass the observed epoch in
`COSMON_WSL2_WINDOWS_BOOT_EPOCH` separately for both phases:

```sh
COSMON_WSL2_WINDOWS_BOOT_EPOCH=<before-epoch> bash "$verify" before-reboot
# Restart the host and launch the distribution.
COSMON_WSL2_WINDOWS_BOOT_EPOCH=<after-epoch> bash "$verify" after-reboot
```

`after-reboot` refuses an equal or earlier value. WSL2 does not start with the
host; explicit distribution launch is part of the phase.

Host sleep and resume with a patrol in flight:

```sh
bash "$verify" before-sleep
# Keep a WSL client attached while the external driver sleeps and resumes the
# host. An idle distribution without a client can stop before sleep.
bash "$verify" after-sleep
```

Finish inside the distribution:

```sh
bash "$verify" final
```

The `after-*` phases require a fresh supervised-child heartbeat, exactly one
expected child, readable JSON state, active services, and the recorded native
manager properties. Each phase records `uptime -s` before and after the
boundary and reports `SURVIVED` only when the distribution boot time is
unchanged; otherwise it reports `RESTARTED`. `after-sleep` refuses a changed
distribution boot time with `distribution stopped before or during sleep` and
requires the in-flight detached patrol to finish within 150 seconds of the
probe start. The timer phase allows 90 seconds for the next one-minute
activation and reports observed load or suspend delays instead of weakening the
assertion.

For remote access to the disposable host, do not bind the remote shell server
only to an address that appears after a network overlay starts. That ordering
can prevent the server from starting at boot even when cosmon's units are
healthy. Bind normally and enforce the access restriction in the host firewall.

## Interpretation

A checkpoint proves only its named phase. Missing checkpoints are pending, not
passes. `final` refuses until all four external `after-*` checkpoints exist and
lists every missing checkpoint. An unreachable host, unfinished lifecycle, or
power action not yet run must remain explicit in the measurement report.
Candidate success establishes candidate behavior at the recorded revision and
hashes; release support still depends on rerunning the same procedure against
the exact published assets and served installer.
