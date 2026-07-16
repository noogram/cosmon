# Tmux paste-buffer — per-call unique name invariant

## The bug (night of 2026-04-13 → 14)

Two workers, A and B, dispatched in parallel. Operator sees worker B's
prompt appear inside worker A's tmux pane. Chaos.

Root cause: `TmuxBackend::send_input` was loading into a paste-buffer
named by the literal string `"cosmon-input"`. **Tmux paste-buffers are
scoped per server (socket) and named in a server-global namespace.** The
shared literal raced across parallel `send_input` calls:

```
thread A                       thread B
────────────                   ────────────
load-buffer "cosmon-input"     (A's buffer not yet pasted)
                               load-buffer "cosmon-input"   ← overwrites A's
paste-buffer "cosmon-input"    ← pastes B's payload into A's session
into session-A
```

Diagnosed in [`delib-20260414-6d73`](./lore/). Fix in commit
[`1360bbc`](https://github.com/).

## The invariant

> **Every paste-buffer is minted by `TmuxBackend::load_buffer` with a
> name derived from `WorkerId` + PID + an atomic counter, and is handed
> back inside a `TmuxBuffer` RAII handle. The handle carries the target
> session name; `TmuxBackend::paste_buffer` pastes into that session and
> consumes the handle. Drop deletes any unconsumed buffer.**

Source:
[`crates/cosmon-transport/src/tmux.rs`](../crates/cosmon-transport/src/tmux.rs)
(module header).

## Mechanism

```rust
static BUF_COUNTER: AtomicU64 = AtomicU64::new(0);

let n = BUF_COUNTER.fetch_add(1, Ordering::Relaxed);
let name = format!(
    "cosmon-input-{}-{}-{}",
    worker.name(),
    std::process::id(),
    n,
);
```

Three uniqueness guards, defence in depth:

1. **`worker.name()`** — disambiguates buffers belonging to different
   workers on the same socket.
2. **PID** — disambiguates across processes sharing the same socket
   (multiple `cs tackle` invocations).
3. **`BUF_COUNTER` (process-global atomic)** — disambiguates concurrent
   calls inside one process. Deterministic across threads (unlike a
   timestamp).

The temp file used as staging for `load-buffer` shares the same unique
name, so same-process threads cannot collide on the filesystem either.

### `TmuxBuffer` RAII handle

`load_buffer` returns a `TmuxBuffer` struct that carries both the
freshly-minted buffer name and the target session name. The only paste
path (`paste_buffer`) takes ownership of the handle — so the paste
target is structurally coupled to the buffer that was minted for it.
**There is no way in the type system to paste buffer X into session Y.**

On failure (load or paste error), the handle's `Drop` impl calls
`delete-buffer` best-effort so a partially-loaded buffer cannot leak
into a later paste of a different session.

## CI enforcement

`.github/workflows/ci.yml` runs a grep gate named
**"Forbid shared tmux buffer name"** that fails the build if the bare
literal `"cosmon-input"` (not a `format!` of it) reappears in the
transport crate. Re-introducing the bug requires deleting the CI rule,
which makes the regression visible in review.

## See also

- [architectural-invariants §Tmux socket model](architectural-invariants.md)
- [`delib-20260414-6d73`](./lore/) — diagnosis deliberation
- [`crates/cosmon-transport/src/tmux.rs`](../crates/cosmon-transport/src/tmux.rs)
