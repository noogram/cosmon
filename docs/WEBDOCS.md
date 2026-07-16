# Web Docs — docs.noogram.org

The Noogram documentation site is an [mdBook](https://rust-lang.github.io/mdBook/)
built from `docs/book/` and published to **Cloudflare Pages** (project
`cosmon-docs`) under a **single custom domain**: **docs.noogram.org**. It is
served over HTTPS with Cloudflare-managed TLS.

The site documents the whole Noogram distribution (whose open-source kernel is
cosmon), which is why the domain is `docs.noogram.org` rather than a
cosmon-specific host — the `docs.*` convention scales to the distribution's other
components. Those components become **sections of this one site** when they ship,
never per-tool sub-sites or sub-domains (topology A, delib-20260711-8d00). Today
the site is all cosmon — the flagship kernel section. **cosmon.dev,
cosmon.noogram.dev, and docs.noogram.dev are no longer hosting targets**
(delib-20260711-8d00, superseding the earlier 2026-07-11 `.dev` choice);
`noogram.dev` is registered defensively and 301-redirects to `noogram.org`. The
Cloudflare Pages project keeps its internal name `cosmon-docs`.

```
docs/book/src/**.md ──mdbook build──▶ docs/book/book/ ──wrangler pages deploy──▶ Cloudflare Pages (cosmon-docs) ──CNAME──▶ docs.noogram.org
```

This document is the runbook. The in-repo pieces it describes:

| File | Role |
|------|------|
| `docs/book/book.toml` | mdBook config (title, navy theme, search, mermaid preprocessor, repo edit links) |
| `wrangler.toml` | Cloudflare Pages config (`name = cosmon-docs`, `pages_build_output_dir = docs/book/book`) |
| `docs/book/src/SUMMARY.md` | Table of contents — a [Diátaxis](https://diataxis.fr/) spine: **Introduction**, then four sections (see the page list below) |

### Book page list (mirrors `SUMMARY.md`)

The book follows the [Diátaxis](https://diataxis.fr/) four-quadrant spine
(Tutorials · How-to · Reference · Explanation) plus a thin Introduction. The
Reference section is **generated from the `cs` clap tree** and CI-golden-checked
(see `docs/book/src/reference/` and `tests/help_goldens.rs`); the rest is
hand-written prose. Keep this list in sync with `docs/book/src/SUMMARY.md` — that
file is the source of truth.

- **Introduction** — `introduction.md`
- **Tutorials** — `setup`, `first-molecule`, `first-fleet`, `first-dag`
- **How-to guides** — `recover-crashed-agent`, `temperature-tags`,
  `bootstrap-project`, `germinate-from-spore`, `monitor-with-peek`,
  `external-scheduler`
- **Reference** (generated) — `overview`, `lifecycle`, `fleet`, `execution`,
  `project`, `observability`, `integrity`, `tools`, `formulas`, `exit-codes`
- **Explanation** — `physics-vocabulary`, `stateless-cli`,
  `control-vs-data-plane`, `regimes`, `crash-recovery`, `cosmon-and-noogram`,
  `architecture`, `versioning`

> **Mermaid.** The book renders Mermaid diagrams client-side. `book.toml` declares
> the `mdbook-mermaid` preprocessor and ships `mermaid.min.js` + `mermaid-init.js`
> as `additional-js`. If you add a diagram, write a fenced ```` ```mermaid ````
> block — `mdbook build` turns it into a `<pre class="mermaid">` the JS renders.

---

## PUBLICATION GATE — read before deploying

Publishing to docs.noogram.org is an **operator gesture**. There is intentionally
**NO** auto-deploy-on-push GitHub Actions workflow for this site — merging the
book pages to `main` must **NOT** flip the site. The prod flip stays under the
operator's finger, via the manual `wrangler pages deploy` below. The custom
domain (`docs.noogram.org`) is attached by the operator at deploy time —
Cloudflare custom domain + CNAME + zero-trust coverage — not by anything in this
repo.

The gate guards the **git repo**, not the rendered book. `github.com/noogram/cosmon`
is private; the deployed site ships **only the rendered HTML** of the book (the
clean public pages), never the repo's git history. Before any prod deploy, the
content of the rendered pages MUST be confidentiality-clean (no client names,
no private endorsers). The pages currently shipped were written in the Feynman
register and verified clean (no red-list term in rendered output).

> **`edit on GitHub` links 404.** `book.toml` sets `git-repository-url` to
> `https://github.com/noogram/cosmon`, which is private. The per-page "Suggest an
> edit" / GitHub icon links therefore 404 for anonymous visitors until the repo
> is made public. This is expected and harmless — it is not a deploy blocker.

---

## Build locally

```sh
cargo install mdbook mdbook-mermaid   # one-time, if not present
mdbook build docs/book                # → docs/book/book/
mdbook serve docs/book                # live-reload preview at http://localhost:3000
```

The generated `docs/book/book/` directory is a build artifact (git-ignored),
rebuilt on every deploy.

---

## Deploying (manual — the only path)

```sh
mdbook build docs/book

# Hard offline validation of local files and heading anchors; HTTP(S) issues warn.
./scripts/check-book-links.sh
# Deploy. Run from the repo root: wrangler.toml's pages_build_output_dir
# already points at docs/book/book, so pass NO directory argument
# (passing the dir AND having the config set double-joins the path and fails):
wrangler pages deploy --project-name=cosmon-docs --branch=main
```

`--branch=main` publishes to the **production** deployment (the one mapped to the
custom domains). Use a different `--branch=<name>` for a preview URL instead.

---

## Post-deploy QA — HEADLESS browser only

After a deploy, two gates confirm the live site is healthy:

**Non-visual gate (pilot-side `curl` — always valid, no browser):**

```sh
curl -sS -o /dev/null -w '%{http_code} %{ssl_verify_result}\n' https://docs.noogram.org/
curl -s https://docs.noogram.org/ | grep -i -E 'noogram-internal|<client-name>'   # leak grep → expect no match
```

This confirms HTTP 200, valid TLS, and absence of red-list terms — but it
**cannot** confirm the Mermaid diagrams render, because they render
**client-side** (the HTML ships a `<pre class="mermaid">`; the browser's JS
turns it into SVG). To verify the render you need a browser.

**Visual gate (client-side Mermaid render — needs a real browser):** use a
**HEADLESS** one.

```sh
chrome --headless --screenshot=/tmp/docs-noogram-org.png \
  --window-size=1280,2000 --virtual-time-budget=4000 https://docs.noogram.org/
# then READ the PNG: did the mermaid blocks become rendered diagrams (not raw code)?
```

…or the `playwright-headless` MCP server (one isolated Chromium per session).

> ⛔ **Never `playwright-extension` from a fleet worker.** That MCP drives the
> *operator's* logged-in Chrome through a browser extension; a headless tmux
> worker has neither, so the call **can never return** — it hangs forever and
> the worker never reaches `cs evolve`. This is exactly how a cosmon-docs deploy
> worker hung 50+ min on one `Calling playwright-extension…` while the deploy
> itself had already succeeded (`task-20260617-6ae2`,
> [chronicle 2026-06-17](lore/CHRONICLES.md)). `playwright-extension` is
> operator-cockpit-only. Deploy/QA workers use headless Chrome or
> `playwright-headless`, full stop.

---

## Custom domain — operator setup at deploy time

The `noogram.org` zone lives on the operator's Cloudflare account, so DNS and
Pages are the same account. The custom-domain attach is an operator gesture done
at deploy time (custom domain + CNAME + zero-trust coverage); this repo carries
no automation for it. Provisioning the `noogram.org` zone itself and the
`noogram.dev → noogram.org` 301 redirect are **C2's remit** (domain/DNS), not
this runbook's — the steps below assume the zone already exists. The steps, for
reference:

1. **Create the Pages project** (once)

   ```sh
   wrangler pages project create cosmon-docs --production-branch=main
   ```

2. **Attach the custom domain** (Pages API — `pages:edit` token scope):

   ```
   POST /accounts/{account_id}/pages/projects/cosmon-docs/domains  {"name":"docs.noogram.org"}
   ```

   (account_id `00000000000000000000000000000000`, read via `wrangler whoami` or
   the cloudflare-api MCP.)

3. **Create the proxied CNAME record.** For an in-account zone, attaching the
   domain *should* auto-provision the CNAME; when it stays `pending`, create the
   record explicitly (orange-cloud / proxied):

   | Zone | Type | Name | Content | Proxied |
   |------|------|------|---------|---------|
   | noogram.org | CNAME | `docs.noogram.org` | `cosmon-docs.pages.dev` | yes |

   This is a subdomain record and does **not** touch the zone's apex `MX` /
   `TXT(SPF)` / `TXT(DKIM)` records — email routing on `noogram.org` is
   unaffected.

Cloudflare then validates the domain and issues a Google-managed TLS certificate
automatically (status `pending` → `active`). The raw `cosmon-docs.pages.dev` URL
works immediately; the custom domain goes live once validation + cert issuance
complete (typically a few minutes).

---

## Troubleshooting

- **`ENOENT … docs/book/book/docs/book/book`** — you passed the directory arg
  *and* `wrangler.toml` has `pages_build_output_dir`. Drop the directory arg.
- **`wrangler pages deploy` says project not found** — run the create step.
- **Custom domain stuck `pending`** — confirm the proxied CNAME exists in the
  zone (step 3). The `*.pages.dev` URL works before the custom domain does.
- **404 / SSL error at docs.noogram.org** — DNS or cert not propagated yet; wait
  and recheck `GET /accounts/{id}/pages/projects/cosmon-docs/domains` for
  `status: active`.
- **`mdbook: command not found`** — `cargo install mdbook mdbook-mermaid`.
- **Mermaid shows as raw code** — `mdbook-mermaid` not installed, or the
  `additional-js` / `[preprocessor.mermaid]` block missing from `book.toml`.
