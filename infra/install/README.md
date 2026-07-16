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
| [`../../.github/workflows/release.yml`](../../.github/workflows/release.yml) | The release matrix (pre-existing; C3 added the `linux-arm64` leg). Builds `macos-arm64/x64` + `linux-x64/arm64`, signs each (cosign + Rekor + SBOM), and cuts a GitHub Release with an aggregate `SHA256SUMS`. |

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

## Scope

This child produces **config + script**, not a deploy. It does **not** flip the
repo public, tag a release, run `wrangler deploy`, or touch a live registrar —
all reserved operator gestures (`CLAUDE.md`: *never run `just install` / deploy
from a worktree*).
