# ADR-144 — Sovereign distribution: a transport-agnostic boot contract, two conformant implementations

**Status:** Accepted
**Date:** 2026-07-06.
**Decider:** Noogram.
**Author molecule:** `task-20260705-8771` (C7 of the decomposition below).
**Parent deliberation:** `delib-20260705-7288`
— *"Sovereign cosmon on LPTHE Jussieu."* This ADR ratifies **D2** of that
panel (architect Q2 verdict: *consolidate the CONTRACT, not the ARTIFACT*) and
carries forward its **security caveat** (Q3) as a hard gate.
**Kind:** ADR-grade because it reserves a federation-wide *noun* — the **boot
contract** — that spans two physically different distribution artifacts, and
because the security carve-out it names (ship-unencrypted-is-single-host-only)
is a structural boundary, not an implementation detail. Per CLAUDE.md —
*"Do not backdoor architectural changes through individual PRs"* — the
consolidation is filed as a decision. **This ADR ships no code**; it names an
abstraction that two already-shipped artifacts (`dist/avatar-tenant-demo/`,
`scripts/lpthe/`) both instantiate.

**Binds / cites / complies with:**

- [ADR-141](141-auto-provisioning-images.md) — *Auto-provisioning images
  (Forgejo + cosmon-server), no external init script.* This ADR **extracts**
  the transport-agnostic contract latent in ADR-141: strip ADR-141 of its two
  container-specific concerns (crypto-at-rest via volume ownership, OCI
  packaging) and what remains — *immutable base, idempotent self-config at
  boot, writes only to scratch/tmpfs* — is the reusable boot contract. ADR-141
  stays the canonical spec of the OCI implementation; it is not superseded.
- [ADR-142](142-incarnation-launch-time-decision.md) — the `Incarnation`
  bundle. A booted instance still spawns workers through the same
  `(adapter, model, effort)` decision; the boot contract provisions the host,
  it does not change how a worker is incarnated.
- [ADR-052](052-one-ledger-one-writer-one-witness.md) — *One Ledger, One
  Writer, One Witness per Field.* The single-writer ledger guarantee is the
  reason the LPTHE implementation may **never** place live `.cosmon/state` on
  NFS (NFS `flock`/`fcntl` make single-writer a lie). The boot contract's
  "writes only to fast local scratch" invariant is this ADR's obligation, not
  an optimisation.
- [ADR-131](131-statestore-port-locking-paths.md) — StateStore port locking &
  paths. Same load-bearing reason as ADR-052: the locking discipline welded to
  the FileStore adapter assumes a POSIX-faithful local filesystem.
- [ADR-030](030-cosmon-archive-model.md) — `.cosmon/state` is gitignored. Both
  implementations rely on this: the OCI image mounts state on a volume, the
  LPTHE build symlinks it to local NVMe — in both cases the live-state path is
  invisible to git, so neither packaging nor provisioning dirties the tree.

---

## Context

Two independent deployments needed cosmon to **configure itself at boot** on a
host the operator does not fully control:

1. **Tenant-Demo avatar** (`dist/avatar-tenant-demo/`, ADR-141). A customer runs cosmon
   + a Forgejo IdP as OCI containers under a LEAN.md working agreement:
   read-only rootfs, writes to named volumes / tmpfs only, minimal Compose
   handed over, per-galaxy config via API only. The images provision their own
   admin account, OAuth2 app, and trust state at boot, idempotently.

2. **LPTHE Jussieu / g5** (`scripts/lpthe/`, C2 of `delib-20260705-7288`).
   cosmon runs on a borrowed academic GPU box as an *invited guest*: no root,
   no `/etc/subuid` (podman rootless is dead on arrival), `/home/tmp` is local
   NVMe scratch, `$HOME` is NFS. A fully-static `x86_64-unknown-linux-musl`
   `cs` binary plus an idempotent `provision.sh` converge an instance with no
   container runtime at all.

These look like two different problems. They are the **same problem** wearing
two bodies. The panel's D2 verdict: forcing them into one physical artifact is a
false economy — a single OCI package would require a container runtime on g5,
which collides head-on with the no-root / no-subuid reality. What they genuinely
share is not the *artifact* but the *contract*: the sequence of guarantees a
cosmon host must satisfy for a worker to be able to trust its own state,
regardless of how the bytes arrived.

Naming that contract is the point of this ADR. Once named, "the LPTHE build" is
no longer a bespoke script — it is a **container-less avatar**: a second
conformant implementation of one doctrine.

---

## The boot contract (transport-agnostic)

A distribution **conforms** to the cosmon boot contract iff it upholds four
invariants. The invariants say nothing about containers, encryption, or the
transport that delivered the bytes — those are divergence points (below).

### BC-1 — Immutable base

The executable substrate is delivered as a **read-only, content-addressed
artifact** and is never mutated in place at runtime. The base is fully
reconstructible from source; its identity is pinned.

- OCI image: the image layers are immutable; the rootfs is mounted read-only
  (LEAN.md). Identity = image digest.
- musl binary: the `cs` binary is fully static (no glibc dependency on an
  unknown/unmodifiable host) and its BLAKE3 is pinned in `MANIFEST.txt` at build
  time; `provision.sh` **re-verifies the seal at boot** and dies loudly on
  mismatch (corrupt/tampered transfer). Identity = BLAKE3 digest.

The corollary shared by both: **the code is transport, the formulas + agent
defs are the IP and travel as data** (D1). Neither implementation bakes
behaviour into the base that cannot be reconstructed from source.

### BC-2 — Idempotent self-config at boot

Provisioning is **convergent**: each step checks its own post-condition first
and is a no-op when already satisfied. Running boot twice equals running it
once. No external init step, no separate orchestrator, no `depends_on` ordering
is required for correctness — a conformant instance heals itself under a
restart policy.

- OCI image: a background provisioner inside the container waits for the local
  IdP, creates admin/OAuth2/trust state, and rewrites handoff/trust files only
  on drift; every foreign entry is preserved (merge-preserving). Convergence is
  fail-closed (parse-back before atomic replace; refuse-to-boot on failure).
- musl binary: `provision.sh` converges the state symlink, formula/skill wiring,
  `cs init`, and the cold-copy mirror — each guarded by its own idempotence
  check. `--check-only` reports preconditions and mutates nothing.

### BC-3 — Writes only to scratch / tmpfs / volume; base stays clean

Live, mutable state is written **only** to a fast, single-writer-faithful,
non-base location. The immutable base is never a write target. Crucially, the
write location must give the single-writer ledger guarantee (ADR-052/ADR-131) a
POSIX-faithful substrate — an NFS or overlay-over-NFS target that silently
breaks `flock` is **non-conformant**, because it turns the ledger's
one-writer promise into a lie.

- OCI image: state on named volumes / tmpfs; rootfs read-only.
- musl binary: `.cosmon/state` is a **symlink to `/home/tmp/$USER/…`** on local
  NVMe (btrfs, confirmed by the C1 preflight probe), never NFS. Because
  `/home/tmp` is reboot-wipeable scratch, durability of *live state* is a
  cold-copy rsync mirror to NFS `$HOME` every ~5 min; durability of *history*
  is git. The symlink (not `COSMON_STATE_DIR`) is chosen because the cosmon
  tmux server freezes its environment at creation — an env var exported before
  `cs tackle` is silently dropped for later worker sessions, whereas a
  filesystem symlink is honoured by walk-up discovery regardless (see CLAUDE.md
  *"tmux server env frozen at start"*).

### BC-4 — Self-verifying, fail-loud preconditions

Before mutating anything, a conformant boot **asserts the facts its correctness
depends on** and refuses to proceed (or warns loudly) when they do not hold.
The contract does not assume its environment; it probes it.

- OCI image: refuses to boot if the trust declaration fails parse-back; the
  admin-username guard fails loudly against Forgejo's reserved names.
- musl binary: asserts the binary seal, that the state fs is local (not NFS),
  the `noexec` status of the mount, and ollama reachability, before it writes.
  Invited-guest discipline is part of this invariant: **the boot escalates
  nothing** — no sudo, no `/etc` writes, no network scanning (*"don't use your
  framework to hack the lab"*).

---

## Two conformant implementations

| Concern | OCI image — `dist/avatar-tenant-demo/` (Tenant-Demo) | musl binary + `provision.sh` — `scripts/lpthe/` (LPTHE) |
|---|---|---|
| **Transport** | OCI image pull (`Containerfile`, Compose) | `scp -J tycho` versioned tarball → `/home/tmp/cosmon/bin/` |
| **BC-1 immutable base** | image digest, read-only rootfs | static musl `cs`, BLAKE3 pinned in `MANIFEST.txt`, re-checked at boot |
| **BC-2 self-config** | in-container provisioner + `trust converge` | `provision.sh` converge (idempotent, `--check-only`) |
| **BC-3 write target** | named volumes / tmpfs | `.cosmon/state` symlink → local NVMe `/home/tmp`; rsync mirror → NFS |
| **BC-4 preconditions** | parse-back fail-closed, reserved-name guard | fs-local / noexec / seal / ollama probes, no escalation |
| **Runtime** | container engine (Podman/Docker) | native `systemd --user` timer or guarded nohup loop |
| **Encryption at rest** | volume-level, engine-provided (available) | **none** (see divergence + security gate) |
| **Containerisation** | yes (its reason to exist) | **no** (a daemon-free CLI in a container is self-contradictory ceremony) |

The two share **one contract and zero code**. That is the intended shape:
"one notion, two artifacts." Consolidating further — forcing a single physical
package — would re-import the exact `subuid`/NFS problems the LPTHE build exists
to avoid.

---

## Named divergence points

The contract is deliberately silent on two axes. They are **divergences by
design**, not gaps — each implementation picks a point and the ADR names the
consequence.

### DV-1 — Encryption at rest is OPTIONAL

The OCI image can lean on engine/volume-level encryption; the LPTHE build ships
and stores **unencrypted**. This is acceptable **only** for a single trusted,
invited-guest host where the data does not leave the building and the operator
is the sole tenant. The boot contract does not mandate encryption because
mandating it would forbid the LPTHE deployment outright.

### DV-2 — Containerisation is OPTIONAL

The OCI image is containerised (its whole reason to exist); the LPTHE build is
native. The contract is transport-agnostic precisely so that containerisation
is a *packaging choice*, not a *correctness requirement*. Containerising a
daemon-free CLI on a no-root host is ceremony that re-imports subuid/NFS
failure modes — so BC-1..BC-4 are stated without any reference to containers.

---

## ⚠️ SECURITY GATE — dedicated review before multi-tenant / off-box

**This is the load-bearing caveat and it is a hard gate, not advice.**

Shipping cosmon **unencrypted** (DV-1) is safe **only** on the current
single-trusted-invited-guest host (g5), where:

- the operator is the **sole tenant** of the state directory,
- the data provably does not leave the building,
- and the host is trusted by prior arrangement.

The `delib-20260705-7288` panel **deliberately declined a red-team seat** — it
did not analyse the adversarial surface of an unencrypted sovereign
distribution. Therefore:

> **GATE.** Before *any* of the following, a **dedicated security review**
> (a red-team pass with the adversarial seat this deliberation declined) MUST
> be completed and its verdict recorded as a molecule:
>
> - **multi-tenant** use of a single sovereign instance (>1 principal sharing
>   the state directory or the host);
> - **off-box** distribution (shipping the unencrypted build to any host that
>   is not the single trusted invited-guest host it was provisioned for);
> - **network exposure** of the sovereign instance beyond localhost / the
>   operator's own tunnel;
> - promoting the LPTHE "container-less avatar" from a **priced experiment on
>   the sovereign option** to a standing production posture.

Until that review lands, the unencrypted divergence (DV-1) is scoped to exactly
one host by this ADR. Crossing the gate without the review is a structural
breach — file the review molecule, do not widen the blast radius by PR.

---

## Consequences

- The LPTHE build is no longer bespoke: `scripts/lpthe/provision.sh` is
  documented as the **second conformant implementation** of the boot contract,
  and its header already cites this framing ("ADR-141's transport-agnostic BOOT
  CONTRACT minus crypto + containers").
- ADR-141 is **narrowed in interpretation, not superseded**: it remains the
  canonical spec of the OCI implementation; this ADR names the shared contract
  it instantiates.
- A **third** distribution (a future host, a different transport) has a
  checklist to conform to — BC-1..BC-4 — rather than a script to copy. New
  transports are cheap; the contract is the reusable asset.
- The security gate is now a **named precondition** on the state, discoverable
  from the ADR index, so widening the deployment cannot happen silently.
- Encryption (DV-1) and containerisation (DV-2) are recorded as *deliberate*
  divergences, so a future reviewer does not mistake the LPTHE build's missing
  crypto for an oversight.

---

## Coherence checklist (per CLAUDE.md)

1. **Stateless?** Yes — this ADR documents a boot contract; both
   implementations are one-shot convergent provisioning, no daemon in the core
   loop.
2. **Idempotent?** BC-2 *is* the idempotence invariant; twice = once is a
   conformance requirement.
3. **Regime-aware?** Provisioning is an Inert→ready host operation; it does not
   change the Inert/Propelled/Autonomous worker boundaries.
4. **Single perimeter?** The contract names an existing shared shape; it adds
   no new command and no new state store.
5. **Symmetric undo?** Provisioning is convergent and its writes are confined
   to scratch/volume (BC-3); tearing down = removing the scratch/volume, the
   immutable base is untouched.
6. **Runtime-compatible?** The contract is transport-agnostic and says nothing
   that the future resident runtime must contradict.
7. **Worker/human boundary respected?** Boot provisioning is a host/operator
   gesture; workers still self-config only their own worktree.
8. **Security boundary named, not assumed?** Yes — the DV-1 unencrypted
   divergence is fenced by an explicit review gate.

---

## Tattoo

**One contract, two bodies. The bytes' journey is a divergence point; the
guarantees are not. Unencrypted is a single-host privilege — cross the gate
only with the red-team seat this deliberation declined.**
