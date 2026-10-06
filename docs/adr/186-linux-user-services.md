# ADR-186: Linux user services use the existing supervisor boundary

**Status:** Accepted (2026-10-04).

**Decider:** Noogram.

**Amends:** [ADR-050](050-unified-patrol-scheduler.md),
[ADR-053](053-cosmon-daemon-supervisor.md), and
[ADR-095](095-resident-runtime-ifbdd-path.md).

**Tracks:** issue #143, units W1–W6.

## Context

The service installers only knew the macOS user-service manager. The binaries
and their file-backed contracts are portable, so adding another supervisor or
moving lifecycle state into an operating-system service would duplicate an
existing boundary. Linux needs a reversible adapter around the existing
supervisor and scheduler instead.

## Decision

Linux installs per-user units. The daemon supervisor is a simple service with
five-second restart delay, `KillMode=mixed`, and a fifteen-second stop timeout.
The main process receives termination first so its existing graceful cascade
can run; any descendant left after the deadline is removed before a replacement
starts. The service is enabled under `default.target`. Installation validates
the application config and staged unit before replacing the installed unit.
Uninstall removes only the owned unit and retains config, logs, state, other
units, and the account's linger policy.

The scheduler remains a one-shot process fired by an external timer. Its
service uses `Type=oneshot` and the narrow `KillMode=process` cleanup exception
needed for detached patrol parity. The timer fires after sixty seconds and then
sixty seconds after each activation, with one-second accuracy, no randomized
delay, and no persistent catch-up. A running tick is not activated a second
time. Stopping the timer prevents future dispatch; it does not cancel already
detached work. Stopping the service terminates a running wait-mode tick but,
by design, leaves detached patrols with the user manager as their owner. This
exception does not apply to the supervisor.

Measured on the first supported host, a delayed detached child survived its
tick and overlapped later timer activations. The same child under the default
control-group cleanup did not reach its delayed marker. Three timer firings
were observed without simultaneous scheduler main processes; a slow wait-mode
tick suppressed overlapping activation. Reload and uninstall stopped future
timer firings while an already detached child completed, and scheduler config,
state, and unrelated units remained present.

The shared boundary is a small shell helper for unit quoting, validation, and
user-manager reachability. It is not a new installation framework. Paths are
absolute, unit arguments are quoted without a shell, percent specifiers are
escaped, and control characters are refused. The existing Darwin templates
and service scripts remain the other adapter to the same entry points.

The seven issue decisions are:

1. Services restart when the distribution next starts. They do not launch a
   stopped distribution or register an automatic host-login job.
2. The scheduler cleanup exception above is accepted and must be measured.
3. Public distribution will be opt-in through `--with-services`; the default
   remains a client-only install.
4. No stay-awake service ships without a concrete policy. Guest services make
   no host power guarantee.
5. Lifecycle validation uses an operator-owned disposable distribution, a
   configured coding adapter, bounded small tasks, and manual host power acts.
6. The first support claim is WSL2 x86_64, Ubuntu 24.04, systemd, with the
   repository and state on the Linux filesystem. Other combinations remain
   unclaimed until measured.
7. A reproducible manual WSL lane plus ordinary Linux CI is sufficient. The
   pinned WSL witness must be rerun for service or installer changes before a
   release claim.

## Alternatives considered

- A container wrapper adds a second process-lifecycle owner without improving
  the existing file-backed recovery contract.
- A new Linux supervisor duplicates the shipped supervisor and would split
  restart semantics across two implementations.
- System services require privilege and violate the ordinary-user boundary.
- A host keepalive or automatic distribution launcher silently expands the
  power and trust contract.

## Consequences

The transactional CLI remains usable with every service absent. User-manager
availability and linger are explicit prerequisites, never installer mutations.
An active unit proves manager state only; application state remains observable
through cosmon's existing files and commands. Shutdown of the distribution can
still interrupt workers, and no unit promises host wakefulness.

## Evidence

The ordinary Linux manager contract is exercised by the service harnesses. The
[WSL2 validation runbook](../guides/wsl2-host-validation.md) defines the manual
host boundary, and the sanitized
[candidate measurement](../measurements/wsl2-host-validation.md) records the
observed matrix. The candidate passed install, lifecycle, crash recovery,
timer, logout, distribution restart, and real host reboot. The first sleep
attempt instead observed an idle distribution stop, so sleep remains
unmeasured. Version 0.7.3 ships the service bundle; the exact published assets
and served installer still require the same host witness.
