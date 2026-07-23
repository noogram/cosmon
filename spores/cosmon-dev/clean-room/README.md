# The clean-room container discipline (`cosmon-dev` §4)

> One image, three postures, a disposable login, two network modes — and the Mac's
> `~/.claude` is NEVER mounted. This is the blueprint §4 discipline made runnable.
> Register: plain, outgoing-surface (no em dashes).

## Why a container at all

Two facets of one principle (blueprint K1, *le mur*):

1. **Determinism.** A reproduction that reads the model, the network, or the host's
   config is reading a *second oracle* and is no longer deterministic. The
   clean-room bakes everything and, in mechanical mode, closes the network
   (`--network none`). What the test asserts on is a decision cosmon takes
   *before* any oracle is consulted.
2. **Env-fit.** Issues #20 and #21 are not logic bugs; they are *environment-fit*
   bugs that only appear under root, in a container, with the released binary.
   You cannot reproduce them on the operator's Mac. The clean-room *is* the
   environment the tester saw.

## One image, pinned by digest

`Dockerfile` builds a single `debian:bookworm-slim` (glibc) image, **pinned by OCI
digest** (not a moving tag), with:

- `@anthropic-ai/claude-code@2.1.215` exact, `DISABLE_AUTOUPDATER=1` (an autoupdate
  is a silent second version — determinism killer).
- A Rust toolchain pinned to an **exact release** (not `stable`, which moves), and
  every cargo build run `--locked` against the committed `Cargo.lock`.
- apt via a **dated snapshot** so `apt-get` is reproducible.
- the cosmon source injected by `git archive <affected_ref>` + hash, **never the
  live checkout** (the live tree drifts; the archive is the frozen ref the tester's
  version corresponds to).

One image means repro and judge cannot *drift apart*: they differ only at
**runtime** (mounts + which credential is injected), never as two images that age
independently (blueprint K5).

## Three postures, one image

| profile | UID | source | writable | starts | for |
|---------|-----|--------|----------|--------|-----|
| `repro-root` | 0 (cognitive worker demoted to UID 10001) | RO | `/output` | at repo | reproducing #20 (a *root* bug) — root is required because the bug IS a root bug |
| `repro-user` | non-root | RO | `/output` | at repo | reproducing non-root-path regressions |
| `judge-user` | non-root, least-privilege | RO | `/output` only | **outside** the repo | the clean-room review seat — reads the code, not the project's self-description |

`scripts/run-clean-room.sh <profile> <affected_ref> <command...>` selects the
posture. The judge starts *outside* the repo so it reads the code, never the
project's `CLAUDE.md`/`MEMORY.md`/neurion self-narrative (blueprint §6).

## The disposable login — never the Mac's `~/.claude`

The subscription login is done by `claude /login` **inside a disposable volume**
mounted at `~/.claude`, with `ANTHROPIC_API_KEY` empty and a leak-check at startup.
`scripts/disposable-login.sh` creates a throwaway volume, runs the login, and
scrubs it on exit.

**NEVER mounted from the Mac** (the forbidden list, blueprint §4):
`$HOME`, `~/.claude`, `~/.config`, `~/.codex`, `~/.ssh`, `~/.gitconfig`, the host
Cargo caches, the live checkout, the live `.cosmon`, the Docker socket.
`scripts/assert-no-host-mounts.sh` fails closed if any appears in the run args.

## Two network modes — do not confuse them

| mode | flag | when | never |
|------|------|------|-------|
| **mechanical** | `--network none`, everything baked, protocol doubles + PTY transcripts | **every PR** — the deterministic reproduction / regression / routing / UID / filesystem gate | as a substitute for a real upstream canary |
| **upstream-real** | egress allowlist to the pinned auth/inference endpoints | the **release canary** (real OAuth/subscription, real TUI changes) | as a per-PR deterministic gate |

Calling Claude over the network and describing the run as "network isolated" is
**false**. Mechanical mode is the honest per-PR gate; upstream-real is the
occasional canary, never the routine gate. `scripts/run-clean-room.sh` defaults to
mechanical and requires an explicit `--net upstream-real` to open egress.

## Honest boundary

The container reproduces *environment-fit* deterministically; it does **not**
substitute for the human replaying the candidate on their own machine (G11). And a
single container run snapshots once — continuous capture is the always-on `trace`
node's job, not this harness's.
