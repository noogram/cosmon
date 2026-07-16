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
   #         x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu}{,.tar.gz} (+ .sig/.pem/.spdx.json)
   ```

> `linux-arm64` (`aarch64-unknown-linux-gnu`) is **cross-compiled** on the
> x86_64 ubuntu runner (the workflow installs `gcc-aarch64-linux-gnu` and points
> cargo's linker at it). This leg is exercised for the first time by the first
> real tag push — watch that matrix leg on the first release and confirm its
> tarball is in `SHA256SUMS`.

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
