# Runbook — activate the public install one-liner (operator gesture)

**One page. Everything below is prepared; you flip it. A worker never does.**
Decision: [`delib-20260711-8d00`](../../.cosmon/state/fleets/default/molecules/delib-20260711-8d00/outcomes.md) (C3).
Prereq: C2 DNS is applied and `noogram.org` resolves through Cloudflare
([`../cloudflare/RUNBOOK.md`](../cloudflare/RUNBOOK.md)).

```
curl -fsSL https://noogram.org/cosmon/install.sh | sh
        │                        │
        │                        └─ Worker (worker/) serves install.sh as text/plain
        └─ install.sh: uname → GitHub Release tarball → verify SHA256SUMS → ~/.local/bin/cs
```

The whole thing is **staged**. Two operator gestures turn it on, in order.

---

## Gesture 1 — publish the repo + cut the first release

Nothing installs until there is a public release to install *from*.

1. Flip `noogram/cosmon` **public** (the membrane-repair / confidentiality gate
   must be green first — `git grep tenant-demo` etc. clean; see the docs-site
   readiness guard). This is the same gate C1's docs deploy waits on.
2. Tag the first release:
   ```sh
   git tag v0.1.0 && git push origin v0.1.0
   ```
   `release.yml` fans out across the four targets, signs each (cosign + Rekor +
   SBOM), and cuts the GitHub Release with an aggregate `SHA256SUMS`.
3. Confirm the assets landed:
   ```sh
   gh release view v0.1.0 --repo noogram/cosmon --json assets \
     --jq '.assets[].name' | sort
   # expect: SHA256SUMS + cosmon-0.1.0-{aarch64-apple-darwin,x86_64-apple-darwin,
   #         x86_64-unknown-linux-musl,aarch64-unknown-linux-musl}{,.tar.gz} (+ .sig/.pem/.spdx.json)
   ```

> The Linux legs are **static musl** (`*-unknown-linux-musl`) — no glibc/libdbus
> runtime dep, portable to any Linux. `linux-arm64` (`aarch64-unknown-linux-musl`)
> is **cross-compiled** on the x86_64 ubuntu runner via `cargo-zigbuild` (zig is
> both the C toolchain and the aarch64 cross-linker). This leg is exercised for
> the first time by the first real tag push — watch that matrix leg on the first
> release and confirm its tarball is in `SHA256SUMS`.

At this point `install.sh` already works against the release directly — you can
smoke it before the Worker is even deployed:

```sh
COSMON_INSTALL_REPO=noogram/cosmon sh infra/install/install.sh --version v0.1.0
cs --version
```

## Gesture 2 — deploy + un-stage the apex Worker

1. From `infra/install/worker/`, attach the apex by **uncommenting** the
   `routes` block in `wrangler.toml`:
   ```toml
   routes = [
     { pattern = "noogram.org/*", zone_name = "noogram.org" },
   ]
   ```
   Attaching the Worker to the apex creates the proxied apex record — **delete
   the STAGING `org_apex_placeholder` AAAA** from `infra/cloudflare/noogram-dns.tf`
   (or the dashboard) to avoid the conflict the C2 runbook warns about.
2. Deploy staged first (still serves coming-soon, `PUBLISHED=false`):
   ```sh
   cd infra/install/worker && npm install && npx wrangler deploy
   ```
   Verify the door is honest while dark:
   ```sh
   curl -sSI https://noogram.org/cosmon/install.sh | grep -i 'HTTP/'   # → 503
   ```
3. Flip live — set the var and redeploy (or set it in the dashboard):
   ```sh
   # in wrangler.toml: [vars] PUBLISHED = "true"   (then)
   npx wrangler deploy
   ```
   or without editing the file:
   ```sh
   npx wrangler deploy --var PUBLISHED:true
   ```

## Verify (once both gestures are done)

```sh
# the endpoint serves the raw script
curl -fsSL https://noogram.org/cosmon/install.sh | head -3          # → #!/bin/sh …

# the full one-liner installs cs end to end
curl -fsSL https://noogram.org/cosmon/install.sh | sh
cs --version

# a pinned version
curl -fsSL "https://noogram.org/cosmon/install.sh?version=v0.1.0" | sh

# the one-gate holds: no endpoint for a tool with no public binary
curl -sSI https://noogram.org/neurion/install.sh | grep -i 'HTTP/'  # → 404
```

## Re-publish the served installer (the anti-drift gesture)

**When:** the `served-drift` CI job reddened, or you changed
`infra/install/install.sh` and the endpoint is live.

The bytes a stranger pipes into `sh` must be **this repo's**
`infra/install/install.sh`, with nothing hand-copied in between. Twice they were
not, and nothing went red either time:

- the gnu→musl target fix had to be re-synced to the served copy by hand;
- v0.2.0 shipped the `cosmon-remote` connector correctly — signed assets on all
  four triples, the client tarball verifiably carrying both binaries — but the
  served installer had **zero** references to `cosmon-remote`. Fresh one-liner
  installs placed `cs` and silently discarded the connector, so the documented
  remote-connect workflow stayed injouable from a fresh public install.

Two mechanisms now stand between us and a third time.

### 1. The installer is a signed release asset (removes the hand-copy)

`release.yml` publishes the tag's own `infra/install/install.sh` as
`cosmon-install-<version>.sh`, cosign-signed and covered by the release
`SHA256SUMS`, exactly like the binaries. It is immutable and derived from
source by construction.

```sh
# what the tag published, and its signature
gh release view "v${VERSION}" --json assets --jq '.assets[].name' | grep cosmon-install
cosign verify-blob \
  --certificate "cosmon-install-${VERSION}.sh.pem" \
  --signature   "cosmon-install-${VERSION}.sh.sig" \
  --certificate-identity-regexp '.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "cosmon-install-${VERSION}.sh"
```

**Prefer serving this asset over any copy.** Two routes, both operator gestures:

- **Worker (already correct):** the Worker in [`worker/`](worker/) imports the
  canonical `install.sh` as text at build time — there is no second copy. A
  `wrangler deploy` after any script edit is all it takes. This is the route to
  use.
- **Any other deploy target:** point it at the release asset — redirect
  `/cosmon/install.sh` to the `cosmon-install-<version>.sh` asset of the latest
  release, rather than storing a copy of the file. If the target can only serve
  static bytes, fetch them from the release at publish time; **never** paste
  from an editor.

> ⚠️ Deploying is an operator gesture. No workflow in this repo pushes to the
> hosting provider, by design.

### 2. The drift detector (the load-bearing piece)

Publishing correctly is not enough — a signed asset nobody points at fixes
nothing, and both incidents were silent. So the conformance check is what
actually holds:

```sh
# what CI runs (served-drift job in .github/workflows/install-lint.yml)
scripts/release/check-install-drift.sh \
  --served-url https://noogram.org/cosmon/install.sh
```

It fails when the served bytes differ from `infra/install/install.sh` — first
naming any missing capability from
[`served-conformance.txt`](served-conformance.txt) in human terms ("the served
copy has no `cosmon-remote` logic"), then diffing byte-for-byte to catch drift
no marker anticipated. It self-skips **only** on a dark endpoint (non-200), and
says so loudly in the job summary.

Run it yourself right after any deploy — that is the gesture that closes the
loop:

```sh
scripts/release/check-install-drift.sh --served-url "$ENDPOINT" && echo "served == source"
```

The detector's own red path is tested offline on every PR
(`scripts/release/check-install-drift.test.sh`), including a reconstruction of
the real v0.2.0 divergence — a served copy that installs `cs` perfectly well and
drops the connector without a word. A drift detector nobody has seen fail is
indistinguishable from one that cannot.

## Rollback

- **Dark the endpoint fast:** set `PUBLISHED=false` and redeploy (or edit the var
  in the dashboard) — instant honest 503, no DNS change.
- **Detach entirely:** remove the `routes` block, `wrangler deploy`, and restore
  the `org_apex_placeholder` record. The apex resolves again to the staging
  placeholder.
- **Bad release:** `gh release delete v0.1.0` (and delete the tag). `install.sh`
  reads the release manifest live, so pulling the release makes `latest` fall
  back to the previous one (or 404 honestly if it was the first).

## Guardrails

- **Never** `wrangler deploy` or tag a release **from a worktree / from CI** —
  both are operator gestures (`CLAUDE.md` publication gate). There is deliberately
  no auto-deploy workflow for the Worker.
- The Worker serves the **single canonical** `infra/install/install.sh` (imported
  as text at build time). Edit the script there; a redeploy picks it up. Do not
  fork a second copy into `worker/`.
- **Never hand-copy `install.sh` into a deploy target.** Serve the signed
  release asset, or let the Worker import the canonical file. A hand-copy is the
  exact mechanism behind both silent drifts; `served-drift` now reddens on it,
  but the cheaper fix is not creating the copy.
- After **any** deploy that touches the installer, run the drift check against
  the live endpoint (above). It takes a second and it is the only thing that
  looks at what strangers actually receive.
