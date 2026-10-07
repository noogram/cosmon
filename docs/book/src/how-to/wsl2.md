# Run cosmon services under WSL2

Cosmon v0.7.3 contains the Linux user-service bundle and the public installer
accepts `--with-services`. The measured baseline is WSL2 on x86_64 Ubuntu
24.04, with systemd enabled and the repository, home directory, and cosmon
state on the Linux filesystem. The candidate passed installation, lifecycle,
crash recovery, timer, logout, distribution restart, and real host reboot
checks. Host sleep has not passed: on the measured host a Modern Standby of
about 26 minutes restarted the distribution, with a WSL client attached. The exact v0.7.3 published assets and served
installer still need the same host run, so this is a measured candidate
contract rather than a completed release witness.

## Prerequisites

Inside the distribution, confirm that systemd is PID 1 and that the user
manager is reachable:

```sh
ps -p 1 -o comm=
systemctl --user show-environment
git --version
tmux -V
```

You also need a configured execution adapter. Keep repositories and
`.cosmon/` state in the Linux filesystem, not a mounted host drive. If the
user-manager command fails, enable systemd for the distribution, restart the
distribution, and try again before installing services.

## Install and inspect the services

The default installer remains client-only. Opt into the supervisor and
scheduler timer explicitly:

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh -s -- --with-services
export PATH="$HOME/.local/bin:$PATH"

$HOME/.local/libexec/cosmon/install-daemon-supervisor.sh status
$HOME/.local/libexec/cosmon/install-scheduler.sh status
```

The installation uses these paths:

| Purpose | Path |
| --- | --- |
| Commands | `~/.local/bin/` unless `--dir` selects another directory |
| Service helpers and templates | `~/.local/libexec/cosmon/` |
| Supervisor and scheduler configuration | `~/.config/cosmon/` |
| User units | `~/.config/systemd/user/` |
| Application state and default logs | `~/.cosmon/` |

The installer creates only missing empty configuration files. Reinstallation
does not replace existing configuration or state.

## Keep the user manager available after logout

The services are per-user units. To keep that user manager active after the
last shell closes, an administrator can enable lingering for the account:

```sh
sudo loginctl enable-linger "$USER"
loginctl show-user "$USER" -p Linger
```

This is an operator choice, not an installer side effect. Reverse it with:

```sh
sudo loginctl disable-linger "$USER"
```

Disabling linger does not remove the units or cosmon state.

## Power and restart behavior

The units start when the distribution and its user manager start. They cannot
start a stopped distribution, start WSL2 with the host, keep the host awake, or
guarantee that a worker terminal survives distribution shutdown.

After a host restart, launch the distribution explicitly and inspect both
services. Use the host's **Restart** action for this check. A shutdown followed
by power-on can retain the previous host boot through Fast Startup and is not a
reboot witness.

Host sleep is not held on the measured host. After a Modern Standby of about
26 minutes the distribution had been restarted during the standby, although a
WSL client window stayed open. The services came back with the distribution.
A distribution restart ends every process running in it, so work in flight
during a sleep should be expected lost; the test probe had already finished,
so that loss was not observed directly. Keeping a client attached was not
sufficient there. Not measured: a short standby, and what restarted the
distribution. Treat a changed distribution boot time as a distribution restart,
not service survival, and inspect recorded molecule state before redispatching.

The scheduler timer does not replay missed cron slots. An overdue interval job
runs on a later tick according to the scheduler's ordinary wall-time rules.
Always inspect recorded molecule state before redispatching interrupted work.

## Stop or remove the services

The service helpers provide symmetric status and uninstall operations:

```sh
$HOME/.local/libexec/cosmon/install-daemon-supervisor.sh uninstall
$HOME/.local/libexec/cosmon/install-scheduler.sh uninstall
```

Uninstall stops and removes the owned units. It retains configuration, logs,
state, unrelated units, and linger policy. Remove retained files separately
only after deciding that their recovery history is no longer needed.

The sanitized [host measurements](../../../measurements/wsl2-host-validation.md)
state which boundaries were observed. The repository also contains the
[repeatable validation runbook](../../../guides/wsl2-host-validation.md).
