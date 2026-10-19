# ADR-188: A worker is stopped through its supervising parent

**Status:** Accepted (2026-10-20). Increment 1 (host-independent stop
channel) implemented in `task-20261019-3b02`; increment 2 (`HerdrBackend`)
not started.

**Decider:** Noogram (operator decision of 2026-10-20).

**Amends:** the module doctrine of
`crates/cosmon-transport/src/worker_supervisor.rs`, which said the
supervisor introduces "no signal handlers, forwarding, new process group".
The process-group half of that sentence is reversed here; the no-handlers
half is kept.

**Relates to:** [ADR-079](079-worker-spawn-port-and-adapter-contract.md)
§5 obligation 4 (idempotent termination). Design spike
`task-20261019-0350` (options (a)–(e), probes against Herdr 0.9.5,
protocol 22).

## Context

`cs worker-supervise` is the waiting parent of an opt-in Codex Exec launch
(`worker_exit_receipts = true`). It claims a `LaunchIdentity` minted before
spawn, spawns the worker, waits on it, and publishes the wait status as a
receipt. Until now nothing could stop that worker except the transport: tmux
`kill-session` behind a pane/server fingerprint guard. That close HUPs the
supervisor together with the worker, so a stop always degraded the receipt
to `Unknown`.

Cosmon is preparing a second host, Herdr. The spike and the herdr seat's
source reading showed that no identifier exposed by Herdr's JSON API can
carry kill authority for one launch:

- `pane.close` takes a `pane_id` only. A restored pane keeps its `pane_id`
  after a server restart with a fresh shell, and a new workspace can reuse a
  closed `wN:p1` (spike probes 14, 15, 20, 21).
- `terminal_id` is the only per-pane incarnation token. JSON `pane.close`
  does not accept it (probe 16). The herdr seat read at tag v0.9.5 that it is
  allocated from wall-clock microseconds plus a process-local counter
  (`src/terminal/id.rs:16`), so it is not random, and that the direct wire
  `AttachTerminal` request accepts it (`src/protocol/wire.rs:509`) while
  JSON `pane.close` does not.
- `pane.close` does not stop only the foreground process group. The herdr
  seat read at v0.9.5 that it snapshots every PID sharing the child's
  session (`src/pane.rs:1769` calls `platform::session_processes`;
  `src/platform/macos.rs:1121` enumerates PIDs with the same
  `getsid(child_pid)`) and sends HUP, then TERM, then KILL, with a 250 ms
  wait after each of the first two phases (`src/pane.rs:1776`), each signal
  delivered to one positive PID at a time (`src/platform/macos.rs:1175`).
  A `setsid()` descendant escapes, which matches the spike's observation
  (probe 12). A different process group in the same session is killed.
- Herdr's waiter sees the child's `ExitStatus` but reduces it to a coarse
  exit reason, and the public `pane_exited` event carries no status.
- The binary client-shell lane is an exception for server restarts:
  `ClientShellEndpointRequest` carries a `boot_id`
  (`src/protocol/wire.rs:592`), minted from the server PID and epoch
  nanoseconds (`src/server/headless.rs:369`); the server refuses a stale
  boot before mutable dispatch
  (`src/server/headless/endpoint_requests.rs:28`), and that lane permits
  `pane.close` (`src/server/client_commands.rs:22`). It requires client-shell
  mode and an active surface. It conditions a close on the server boot, not
  on the pane's `terminal_id`, so it still cannot tell a reused `pane_id`
  within one boot from the launch's own pane; JSON `pane.close` has no
  condition at all.
- An argv pane's command is not replayed on restore; `resume_argv`, held by the
  reporting process (which need not be a recognised agent), is a separate
  replay path, which types the quoted argv
  into a fresh shell (`src/app/agent_resume.rs:226-315`). A server may ignore unknown
  request keys, so an upstream conditional close needs capability
  negotiation, not only a new optional field.

The herdr seat's full verdicts, against v0.9.5 commit
`2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01`, are in the molecule directory
of `task-20261019-b672` (`herdr-source-findings.md`, not tracked).

A check-then-close on the client (`pane.get`, compare `terminal_id`,
`pane.close`) has a restart window between check and effect. One Herdr
server per worker is addressed by session name, which a restart reuses.
Input-level stops (`send_keys` Ctrl-C) are addressed by `pane_id`.

## Decision

### 1. The stop authority is the supervising parent

A supervised worker is stopped only by asking the `cs worker-supervise`
process that owns its unreaped child handle. The request carries the full
`LaunchIdentity`. The supervisor acts only when that identity is equal to
the identity it holds in memory, and then signals the process group it
created at spawn. Herdr ids, tmux names, pane ids and recorded PIDs never
become stop authority.

The port is `cosmon_core::worker_exit::WorkerStopPort::request_stop(&LaunchIdentity)
-> Result<StopAccepted, StopRefused>`. It is I/O-free in `cosmon-core`. The
Unix-socket client and listener live in
`cosmon_transport::worker_control`.

### 2. Trust boundary

- **Authority is identity equality.** The supervisor compares the requested
  `LaunchIdentity` (schema, molecule, launch id, incarnation, request hash)
  with its own. A partial or different identity is refused as
  `IdentityMismatch` and has no effect.
- **Same UID, checked from the kernel.** Both ends read the peer's effective
  UID from the socket (`getpeereid` on BSD/macOS, `SO_PEERCRED` on Linux)
  and refuse a peer whose UID differs from their own. The client applies the
  same check to the server, so a process of another user cannot answer at
  the rendezvous path. A process of the same user can: it could bind the
  path and reply "accepted". The boundary is the user, as for every other
  file under `~/.cosmon`; a same-user process is already able to kill the
  worker directly.
- **The socket path is a rendezvous, never authority.** It is
  `~/.cosmon/run/w/<first 16 hex of SHA-256 of the identity JSON>.sock`,
  outside the molecule directory because a launch-directory path exceeds
  macOS's `sun_path` limit (spike probe 24). The directory is created with
  mode 0700. The supervisor binds the path after its exclusive launch claim
  and before spawn; a stale file at that path is unlinked by that bind and
  never followed. A client that reaches the wrong process at that path is
  refused by the identity comparison.
- **One request, one reply.** A connection carries one JSON line of at most
  4 KiB and receives one JSON line. Reads are bounded by a timeout. The
  listener never reads a second request on a connection.

### 3. Process group, terminal foreground and hangup

- The supervisor spawns the worker into a new process group whose id is the
  child's PID (`Command::process_group(0)`). That group cannot be
  reallocated while the child is unreaped, and only the supervisor reaps it.
- **Lock ownership.** The child handle, its reaped status, the identity
  comparison and both group signals sit behind one mutex, owned by the
  `OwnedWorker`. `terminate_group` takes an authorisation callback (the
  identity comparison) and runs it under that mutex before checking the
  exited flag, so admission and the exit check are one critical section.
- **Reaping and the `exited` flag share one mutex.** The supervisor polls
  `try_wait` under the mutex and records the exit in the same critical
  section that reaps. The stop path takes the same mutex and refuses with
  `AlreadyExited` once the exit is recorded, so it can never signal a reaped
  (and therefore reusable) group id. The spike proposed
  `waitid(WEXITED | WNOWAIT)`; the safe `nix` binding exposes `waitid` only
  on Linux and `cosmon-transport` forbids `unsafe`, so polling under the
  mutex (20 ms) gives the same property on macOS and Linux.
- **Terminal foreground.** When the supervisor's stdin is a terminal whose
  foreground group is the supervisor's own group, the supervisor hands the
  foreground to the child's group (`tcsetpgrp`) and then sends `SIGCONT` to
  that group, so a child that touched the terminal before the handover and
  was stopped by `SIGTTIN`/`SIGTTOU` resumes. Without a terminal (pipes,
  tests) nothing is handed over. The supervisor never reads the terminal.
- **Stop.** `OwnedWorker::terminate_group` sends `SIGTERM` to the group,
  waits up to a grace period (5 s in production) for the recorded exit, and
  sends `SIGKILL` to the group if it has not exited. Both signals are sent
  under the mutex after re-checking the flag. Descendants in the group
  receive both signals while the worker is unreaped; a `setsid()` escapee
  survives, as under tmux and Herdr. If the worker exits on `SIGTERM`
  within the grace period and is reaped, `SIGKILL` is not sent, because the
  group id is no longer protected by an unreaped leader. A descendant that
  ignores `SIGTERM` then survives. This is a known gap of increment 1.
- **Hangup.** The supervisor installs no signal handlers. When the terminal
  hangs up, the kernel sends `SIGHUP` to the session leader (the supervisor,
  launched through `exec`) and to the foreground group (now the worker's).
  Both die as before, and that path still yields an `Unknown` receipt.
  Cosmon no longer takes that path for a supervised worker: teardown
  happens only after the stop channel has produced a terminal receipt. A
  `SIGINT` typed into the pane now reaches the worker's group only, and the
  supervisor records `Signaled { 2 }`.

### 4. Refusals never fall back

`StopRefused` has four kinds:

| Kind | Meaning |
|---|---|
| `Unreachable` | No listener at the rendezvous path, a peer with another UID, a malformed or missing reply, or a timeout. Includes a supervisor that was killed. |
| `IdentityMismatch` | The supervisor at the path holds a different launch identity. |
| `AlreadyExited` | The child's exit was recorded before the request; no signal was sent. |
| `Unsupported` | Platform without Unix sockets, an unknown request version, or a signal error other than "no such group". |

No refusal falls back to any name-, pane- or PID-addressed effect.
`cs purge --force` and `cs done` step 3 call the stop channel for a
supervised launch (one with `process.worker_launch`) and then:

- if the launch's receipt is terminal (before the request, or after an
  accepted stop or `AlreadyExited` within a bounded wait), they continue
  with the existing incarnation-bound tmux teardown, which now removes a
  dead pane;
- otherwise they warn, kill nothing, and leave the launch unresolved.

`cs purge` still removes the fleet record in both cases, as it already did
for a stale terminal. The unsupervised tmux path is unchanged.

Each request is appended to `stop-requests.jsonl` in the launch directory
(identity, outcome, time). The file is audit evidence only and is never
consulted as authority or as a receipt.

### 5. Relation to ADR-079

ADR-079 obligation 4 requires that terminate be idempotent. A second stop of
the same launch is either `AlreadyExited` (supervisor still publishing) or
`Unreachable` with a terminal receipt (supervisor gone), and both callers
treat a terminal receipt as "already stopped". Neither repeats a signal.

### 6. Residual gap

If the supervisor is `SIGKILL`ed or crashes, its socket file usually stays
behind with no listener, so `connect` is refused, and the worker may keep
running in its own group with no parent to stop it (macOS has no
`PR_SET_PDEATHSIG`). It may also die of a terminal or orphaned-group
hangup; Cosmon does not rely on either. The stop is refused as `Unreachable`, the receipt
stays `Unknown`, and the launch needs operator resolution under the
existing "unresolved launch" rule. Cosmon does not resolve it automatically.

A Herdr-native conditional close (an upstream `expected_terminal_id` on
`pane.close`, checked server-side, combined with the server boot identity
Herdr already has on its client-shell lane) is
recorded as a possible future recovery path for that case only. It would be
invoked explicitly by an operator, never as a fallback of a refused stop.

## Consequences

- A stop of a supervised worker yields its real wait status
  (`Signaled { 15 }`, `Signaled { 9 }` after escalation, or the worker's own
  exit code if it handles `SIGTERM`), instead of `Unknown`.
- The mechanism is host-independent. It stops a supervised tmux worker
  today and is the stop path increment 2 will use under Herdr, where
  `terminate` and `terminate_session` return `Unsupported` and never call
  `pane.close`.
- Qualifying `TransportKind::Herdr` remains increment 2, with the
  conformance list of the spike (§3 "Increment 2").

## Verification

Increment 1 tests: the stop port with fakes (matching identity signals,
mismatch refused, stop after exit refused with no signal); a real child
trapping `SIGTERM` receipts `Exited` with its trap code and a plain child
receipts `Signaled { 15 }`, never `Unknown`; a grandchild in the group
that keeps the default `SIGTERM` disposition dies; a `SIGKILL`ed supervisor gives `Unreachable` and no fallback; two
same-name launches A and B under tmux, stopping A leaves B's process and
receipt intact; a child that reads the terminal under a pseudo-terminal is
not stopped by `SIGTTIN`.
