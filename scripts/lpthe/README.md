# Sovereign cosmon on LPTHE (g5) — build · ship · provision

**C2 of `delib-20260705-7288`** ("Sovereign cosmon on LPTHE Jussieu"). This
directory is the *container-less avatar* (D2): ADR-141's transport-agnostic boot
contract with the crypto and the container stripped off, leaving a static musl
binary plus an idempotent `provision.sh`.

> One notion, two conformant implementations: an OCI image (Tenant-Demo avatar) and
> a musl-binary + `provision.sh` (LPTHE). See the ADR filed as C7.

## The three scripts

| Script | Runs on | Role |
|--------|---------|------|
| `build-cs-musl.sh` | **Mac** | Cross-compile `cs` → `x86_64-unknown-linux-musl` (fully static, rustls/ring — no openssl). Pins BLAKE3 + toolchain into `MANIFEST.txt`. |
| `ship-lpthe.sh` | **Mac** | Pack the versioned tarball (`cs` + formulas + skills + config + provision/backup scripts) and `scp -J tycho` it to `g5:/home/tmp/cosmon/`. Verifies the BLAKE3 seal on the far side. |
| `provision.sh` | **g5** | Idempotently converge an instance: wire local state, symlink formulas, health-check ollama, `cs init`, install the NFS cold-copy mirror, smoke-test. Safe to re-run. |
| `cosmon-state-backup.sh` | **g5** | One rsync cold-copy pass of live state (`/home/tmp` → NFS `$HOME`). Called on a timer by `provision.sh`. |

## Why this shape (the confirmed facts, from the C1 preflight GO)

- **`/home/tmp` = btrfs LOCAL on NVMe** (1.9 TB, ~1.1 TB free), no `noexec`,
  persistent but reboot-wipeable. → live state goes here (single-writer-safe).
- **`$HOME` = NFS (ada:/ada3)** — durable, but NFS `flock`/`fcntl` make the
  ADR-052/ADR-131 single-writer ledger a **lie**. → NFS is backup only, never
  the working ledger.
- **glibc 2.42 x86_64**, unmodifiable without root → **fully static musl** binary
  links nothing on the host.
- **No `/etc/subuid`** → podman rootless is dead → **native binary, no container**
  (D3).
- **ollama at `127.0.0.1:11434`** (native, no tunnel hop) → the sole local oracle.

## State-path decision: **symlink**, not `COSMON_STATE_DIR`

`cs` honours both, but `provision.sh` symlinks the gitignored `.cosmon/state`
(ADR-030) → `/home/tmp/$USER/cosmon-state/<galaxy>/`. The symlink is chosen over
the env var because the cosmon **tmux server freezes its environment at
creation** (see CLAUDE.md "tmux server env frozen at start"): an env var
exported before `cs tackle` is silently dropped for later worker sessions,
whereas a filesystem symlink is honoured by walk-up discovery regardless. A
`cosmon.env` exporting `COSMON_STATE_DIR` is emitted as a belt-and-suspenders
fallback for one-shot `cs` calls outside tmux.

## Quick start

```bash
# On the Mac — build + ship in one shot:
scripts/lpthe/ship-lpthe.sh --build            # or --dry-run to inspect the plan

# On g5 (after the tarball unpacks to /home/tmp/cosmon/dist/<pkg>):
/home/tmp/cosmon/dist/<pkg>/scripts/provision.sh --prefix /home/tmp/cosmon/dist/<pkg>
source /home/tmp/cosmon/dist/<pkg>/cosmon.env
cs --version && cs observe --json
```

`provision.sh --check-only` verifies every precondition (binary seal, local-fs,
ollama) and mutates nothing — run it first on a fresh host.

## Reproducibility without a container (niel Q5)

`MANIFEST.txt` pins the binary's BLAKE3, the `rustc`/`zig`/`cargo-zigbuild`
versions, the git commit, and the target triple. `provision.sh` re-verifies
`cs_blake3` on g5 before running the binary; a corrupt or tampered transfer
aborts the provision. A rebuild on the same toolchain reproduces the hash.

## Invited-guest discipline

Everything here escalates nothing: no `sudo`, no system units, no `/etc` writes,
no network scanning (Dave: *"don't use your framework to hack the lab"*). The
state mirror runs as an unprivileged `systemd --user` timer, or a guarded
`nohup` loop if `systemctl --user` is unavailable.
