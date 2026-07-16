# infra/cloudflare — Noogram public DNS + redirect

Prepared (not applied) Cloudflare config for the Noogram web surface.

| File | Role |
|------|------|
| [`noogram-dns.tf`](noogram-dns.tf) | Versioned source of truth — zones, DNS records, and the two 301 redirect rulesets (Cloudflare Terraform provider v5). |
| [`RUNBOOK.md`](RUNBOOK.md) | One-page operator runbook — dashboard-manual record tables, verification (`curl`/`dig`), and rollback. Apply via Terraform **or** by hand; both reach the same end state. |

## The decision (delib-20260711-8d00)

- **`noogram.org`** is **canonical** — apex serves the project site + install
  endpoints (`/<tool>/install.sh`); **`docs.noogram.org`** serves the doc site.
- **`noogram.dev`** is a **defensive** registration that **301-redirects** to
  `noogram.org` (path + query preserved). No independent content, ever. Docs do
  **not** live on `.dev`.

One brand, one apex; the developer/public split rides the `docs.` subdomain, not a TLD.

## Scope of this child (C2)

Prepares DNS/redirect config **only**. It does **not**:

- touch a live registrar or Cloudflare account (that is an **operator gesture**);
- deploy the apex install worker or the release matrix (child **C3**);
- re-point the docs Pages project (child **C1**).

Applying any of this — nameserver swap, `terraform apply`, dashboard edits — is
the operator's move, held behind the publication gate in `CLAUDE.md`. See
`RUNBOOK.md` § "Sequencing & gates".
