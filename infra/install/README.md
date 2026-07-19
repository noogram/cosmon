# infra/install — the public one-liner installer

Prepared (not live) infrastructure for the provider-standard install command:

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh
```

Child **C3** of the domain-strategy deliberation
[`delib-20260711-8d00`](../../.cosmon/state/fleets/default/molecules/delib-20260711-8d00/outcomes.md).
Built on the C2 DNS/redirect config in [`../cloudflare/`](../cloudflare/).

## The three pieces

| Path | Role |
|------|------|
| [`install.sh`](install.sh) | The installer. `uname` detection → download the matching tarball from GitHub Releases → verify against `SHA256SUMS` → `chmod +x` → install `cs` into `~/.local/bin` → print next steps. Placeholder-free and standalone-runnable. |
| [`worker/`](worker/) | Cloudflare Worker on the `noogram.org` apex serving raw `text/plain` shell at `/<tool>/install.sh`. Imports the single canonical `install.sh` as text — no second copy. **Staged**: gated by a `PUBLISHED` var (default `false` ⇒ honest 503 "coming soon"). |
| [`../../.github/workflows/release.yml`](../../.github/workflows/release.yml) | The release matrix (pre-existing; C3 added the `linux-arm64` leg). Builds `macos-arm64/x64` + `linux-x64/arm64`, signs each (cosign + Rekor + SBOM), and cuts a GitHub Release with an aggregate `SHA256SUMS`. It also publishes **`install.sh` itself** as `cosmon-install-<version>.sh` — signed and checksummed like any binary, so the public endpoint can serve an immutable artifact derived from source instead of a copy. |
| [`served-conformance.txt`](served-conformance.txt) | The capabilities the **served** installer must carry, one substring per line. Each is validated against the canonical script too, so a marker can never rot into a claim the source no longer makes. |

## Served must equal source

The source of truth is `install.sh` here. The bytes a stranger pipes into `sh`
come from the public endpoint. Whenever a **hand-copy** joined the two, they
drifted — and both times it was silent:

1. the gnu→musl target fix had to be re-synced to the served copy by hand;
2. v0.2.0 shipped the `cosmon-remote` connector correctly (signed assets on all
   four triples, client tarball verifiably carrying both binaries), but the
   served installer had zero `cosmon-remote` references — so a fresh one-liner
   install placed `cs` and silently discarded the connector, leaving the
   documented remote-connect workflow injouable from a fresh public install.

Two mechanisms answer that, and only the second is load-bearing:

- **Publishing** — the installer ships as a signed release asset, and the
  Worker imports the canonical file as text. Both routes remove the hand-copy.
- **Conformance** — [`scripts/release/check-install-drift.sh`](../../scripts/release/check-install-drift.sh)
  fetches the served bytes and fails unless they equal `install.sh`. A signed
  asset nobody points at fixes nothing, so the check is what actually holds.
  CI runs it as `served-drift`; its own red path is tested offline on every PR
  by `check-install-drift.test.sh`, which reconstructs the real v0.2.0
  divergence and asserts the detector reddens on it.

```sh
scripts/release/check-install-drift.sh --served-url https://noogram.org/cosmon/install.sh
```

## The one gate (why this design holds)

A `/<tool>/install.sh` endpoint exists **iff** the tool ships a public
per-platform binary — the same gate as the per-tool doc section
(delib Q4 = Q5). `cosmon` is wired because it has a release matrix; every other
path is an honest `404`. The private **neurion** product has no public binary,
so it can **never** acquire an install endpoint — the guard is structural, not a
matter of memory. The `worker/` route table is the mechanical projection of that
rule.

## Publication gate — nothing here is live

Both live moves are **operator gestures** held behind the `CLAUDE.md`
publication gate:

1. flip `noogram/cosmon` **public** + cut the **first tagged release**;
2. deploy the Worker with `PUBLISHED=true` and attach the `noogram.org` apex.

Until both happen the endpoint 503s honestly and `curl -fsSL … | sh` is a safe
no-op (`curl -f` exits non-zero on the 503 and pipes nothing). See
[`RUNBOOK.md`](RUNBOOK.md) for the exact activation steps.

## Verifying locally (no network, no deploy)

```sh
# installer: syntax, lint, platform-detection self-test
sh install.sh --self-test
shellcheck -s sh install.sh

# worker: typecheck + a dry-run build that bundles install.sh as text
cd worker && npm install && npm run typecheck && npm run dry-run
```

The `--self-test` above only checks the uname→triple *mapping* in isolation. To
exercise the **full** resolve→verify→unpack→install path against a throwaway
release without any published assets, point the installer at a `file://`
fixture via `COSMON_RELEASE_BASE_URL` (the same seam CI's non-skippable
`fixture` job uses — see [`../../.github/workflows/install-lint.yml`](../../.github/workflows/install-lint.yml)):

```sh
d=$(mktemp -d); printf '#!/bin/sh\necho "cs 9.9.9"\n' > "$d/cs"; chmod +x "$d/cs"
tar -czf "$d/cosmon-9.9.9-$(sh install.sh --print-target).tar.gz" -C "$d" cs
( cd "$d" && sha256sum cosmon-*.tar.gz > SHA256SUMS )   # shasum -a 256 on macOS
COSMON_RELEASE_BASE_URL="file://$d" sh install.sh --dir "$(mktemp -d)"
```

That path — plus the cross-surface `triples` job that keeps install.sh, the
`release.yml` build matrix, and the packaging formulas in lockstep — is what
catches a uname→triple drift (the gnu/musl class) **before** any release exists,
rather than only via the post-publication weekly schedule.

## Scope

This child produces **config + script**, not a deploy. It does **not** flip the
repo public, tag a release, run `wrangler deploy`, or touch a live registrar —
all reserved operator gestures (`CLAUDE.md`: *never run `just install` / deploy
from a worktree*).
