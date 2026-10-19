<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Supervision portable des workers

Durable exit receipts are an opt-in qualification feature for the POSIX
`cs tackle --adapter codex` tmux path in the existing non-interactive Exec mode.
Enable them in the galaxy configuration:

```toml
[adapters.codex]
mode = "exec"
worker_exit_receipts = true
update = "skip" # or "operator"; automatic worker relaunch is refused
```

The default retains historical supervision and the steerable Interactive mode.
Opt-in with Interactive (including an omitted or unrecognised mode) is refused
before dispatch effects. It does not change the whisper signature guard or add
a new whisper path; interactive workers continue through historical supervision.
Other adapters and non-POSIX platforms refuse the option. There is no Herdr worker backend,
global supervisor service, provider classification, or automatic retry.
The internal `cs worker-supervise` command is transport plumbing; launch
workers through `cs tackle` so dispatch and authority checks run first.

Each dispatch prepares a random launch and incarnation in the molecule's
canonical state directory before starting its pane. The waiting Rust parent
launches the resolved executable directly, inherits cwd, standard input/output,
terminal dimensions and signal dispositions, and waits for that exact child.
It adds no signal handler, process group, PTY proxy, PID-based kill authority,
or adoption mechanism. The adapter preserves its existing credential stripping,
explicit gateway environment, writable roots, model flags and git identity.
The prepared process snapshot carries the exact launch into the existing
terminal compare-and-swap, preserving the read-only terminal capability without
allowing evidence from launch A to be stamped on a replacement B.
The receipts contain a command binding and hashes of declared environment
changes, never the argv, complete environment, or plaintext credentials.
Ambient environment values are inherited as before, not certified by a receipt.

Evidence lives in `worker-launches/<launch>-<incarnation>/` with private
directory/file permissions (0700/0600). `worker-launch.json` pins the expected
identity, also recorded on the process and ledger. An exclusive synced claim
precedes spawn. It is permanent: a concurrent caller or retry cannot start
a second child. Birth and result must match the expected molecule, launch,
incarnation, request binding and parent/child witnesses. PIDs are diagnostic
numbers, not signal capabilities. This trusted private storage is not a
cryptographic attestation against another writer with the same account.

Normal exit 143 remains `exited/code=143`; SIGTERM remains `signaled/signal=15`.
An exec-handshake failure is distinct from either. Publication syncs a temporary
file, renames it atomically and syncs the directory. Temporary, missing,
corrupt or foreign evidence never gives a terminal verdict. Software crash
tests exercise these boundaries; power loss and network filesystems are not
qualified by those tests.

Ctrl-C or terminal/server loss can kill the parent before it publishes its
wait. The honest observation is then **unknown**, even if the child also
stopped. Receipts do not promise supervisor survival. They do not qualify
arbitrary wrappers, daemonized descendants, job-control changes, or provider
behavior. No signal is sent to a PID reconstructed from a receipt.

For supervised launches the pane-died hook is only a launch-pinned trigger.
`cs harvest` ignores its pane code, reads validated evidence, and emits an
idempotent launch observation. A signal is never projected as a normal code.
An exit of zero does not complete a molecule or grant harvest authority:
completion and existing integration authorization remain separate. Missing
supervised metadata never falls back to the historical pane interpretation.
Unknown evidence blocks automatic harvest and redispatch, preserving the
worktree and receipts for the pilot. This increment has no command to resolve
an ambiguous orphan: do not delete a claim or infer resolution from a vanished
PID. A new supervised dispatch is allowed only after terminal evidence exists
and the existing explicit dispatch checks permit it.

Domain, storage, direct-wait, concurrency and six-window crash tests run in
the ordinary suite. Real tmux qualification is run explicitly:

```sh
./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test worker_exit_receipts -- --ignored
```

The initial terminal qualification is macOS only. Linux must run its own
qualification before it is described as qualified; Windows is outside this
POSIX increment. No model request is needed by the fake-worker tests.
