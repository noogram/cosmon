# WSL2 candidate host validation

Date: 2026-10-05. Scope: candidate evidence for issue #143, not a published
release witness.

## Candidate

The candidate was built from revision
`649dcf240882a20233e925fd4a5776deb2035868` for
`x86_64-unknown-linux-musl`. The installed binaries reported version 0.7.2;
the two binaries that expose source provenance reported the same clean revision
without a dirty marker.

| Asset | SHA-256 |
| --- | --- |
| `cosmon-0.7.2-x86_64-unknown-linux-musl.tar.gz` | `e2f95f471695254952b2c6a0c1888ea79a50b1e5f738747798a3c75cef7604b7` |
| `cosmon-service-0.7.2-x86_64-unknown-linux-musl.tar.gz` | `1cd9c50eb193800c135d28663f93ed304f33d962f0804ce70768f5a68d929956` |

Both archives passed checksum verification through the public installer code
before any candidate file was installed.

## Host and prerequisites

The run used an ordinary user in an Ubuntu 24.04.5 WSL2 distribution on an
x86_64 6.6.87.2 kernel. The home directory was on ext4. systemd 255 was PID 1,
the per-user manager was reachable, and lingering was enabled. Git 2.43.0,
tmux 3.4, the configured execution adapter, and an existing Git identity were
available. The witness did not alter the identity or linger policy.

## Observations

The published v0.7.2 installer was run first as the negative control. It
refused `--with-services` as an unknown argument and installed no user units.
This is the expected RED: the published release has no service installation
surface.

The candidate installer then ran with `--with-services` and a separate binary
directory. It installed the client and service binaries, seeded valid empty
configuration for the fresh account, validated both configurations, and
enabled and started the supervisor service and scheduler timer. The observed
manager properties included `KillMode=mixed` for the supervisor and
`KillMode=process` for the scheduler service.

The service baseline used one supervised heartbeat child and one timer patrol.
The supervisor state and scheduler state were readable JSON. Killing the
supervisor main process produced one replacement supervisor and one replacement
child; the old child did not survive. Killing the child produced one fresh
child with a fresh heartbeat. The next timer activation produced a new firing
within the script's 90-second bound.

One lifecycle task was run through the configured adapter. It created and
committed the requested file, reached `completed`, and retained the artifact in
its worker worktree before harvest. `cs done` was then run from the disposable
repository root, outside the worker. The worker commit became an ancestor of
`main`, the file contents matched, and the worker session, worktree, and branch
were absent afterwards.

At handoff, the supervisor service and scheduler timer were active. The
last-shell logout, distribution shutdown and relaunch, host reboot and
distribution launch, and host sleep and resume phases remain pending for the
external driver. Their absence is not a pass. A second run against the exact
published archives and served installer is also required before making a
release support claim.
