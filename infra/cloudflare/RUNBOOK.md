# Runbook — Noogram DNS + redirect (operator gesture)

**One page. Prepared config only — you apply it; a worker never does.**
Source of the record set: [`noogram-dns.tf`](noogram-dns.tf).
Decision: `delib-20260711-8d00` (`.org` canonical, `.dev` defensive 301).

```
noogram.org  ── apex ─────▶ project site + /<tool>/install.sh   (Worker/Pages, child C3)
noogram.org  ── docs. ────▶ doc site  (Pages project cosmon-docs, child C1)
noogram.dev  ── * ────────▶ 301 → https://noogram.org/<same path>?<same query>
```

Two ways to apply — pick one. Both stop at the same end state.

- **Terraform:** `export CLOUDFLARE_API_TOKEN=… ; terraform init ; terraform plan` →
  read it → `terraform apply`. Then do **step 1** (registrar) with the printed
  `*_nameservers` outputs.
- **Dashboard:** follow steps 1–4 below and type the record table by hand.

---

## Step 1 — Add both zones, point the registrars (activates DNS)

1. Cloudflare dashboard → **Add a site** → `noogram.org` (Free plan is fine). Repeat for `noogram.dev`.
2. Cloudflare shows **two nameservers** per zone. At each domain's **registrar**, replace the nameservers with Cloudflare's.
3. Wait for each zone to flip **Active** (minutes to a few hours). ← *this is the only irreversible-ish step; everything below is inside Cloudflare and trivially editable.*

> If `noogram.dev` isn't registered yet: register it first (defensive), then add it as a Cloudflare site.

## Step 2 — DNS records

**Zone `noogram.org`**

| Type | Name | Content | Proxy | TTL | Why |
|------|------|---------|-------|-----|-----|
| AAAA | `noogram.org` | `100::` | **Proxied** | Auto | STAGING placeholder for the apex. **Delete when C3 attaches its Worker/Pages custom domain** (that attach creates the real apex record). |
| AAAA | `www` | `100::` | **Proxied** | Auto | Gives the `www → apex` redirect (step 3) traffic to act on. |
| CNAME | `docs` | `cosmon-docs.pages.dev` | **Proxied** | Auto | Doc site. **May be auto-created** when child C1 attaches `docs.noogram.org` to the `cosmon-docs` Pages project — if so, skip this row. |
| TXT | `noogram.org` | `v=spf1 -all` | DNS only | Auto | No mail sent from apex → anti-spoof. |
| TXT | `_dmarc` | `v=DMARC1; p=reject; sp=reject; adkim=s; aspf=s;` | DNS only | Auto | Reject spoofed mail. |
| MX | `noogram.org` | `.`  (priority `0`) | DNS only | Auto | RFC 7505 null-MX — "accepts no mail". |

**Zone `noogram.dev`** (redirect-only, no origin)

| Type | Name | Content | Proxy | TTL | Why |
|------|------|---------|-------|-----|-----|
| AAAA | `noogram.dev` | `100::` | **Proxied** | Auto | Black-hole apex; the edge terminates and the redirect (step 3) fires. |
| AAAA | `www` | `100::` | **Proxied** | Auto | `www.noogram.dev` also redirects. |
| TXT | `noogram.dev` | `v=spf1 -all` | DNS only | Auto | Anti-spoof on a parked domain. |
| TXT | `_dmarc` | `v=DMARC1; p=reject; sp=reject; adkim=s; aspf=s;` | DNS only | Auto | Reject spoofed mail. |
| MX | `noogram.dev` | `.`  (priority `0`) | DNS only | Auto | Null-MX. |

*The proxied `100::` rows must be orange-cloud (Proxied) — a redirect rule only runs on proxied traffic. `100::` is Cloudflare's documented discard address; nothing is hosted there.*

## Step 3 — Redirect rules (Rules → Redirect Rules → "Single Redirects")

**In zone `noogram.dev`** — *Create rule* → name `dev → org`:
- **When:** Custom filter → expression: `(http.host eq "noogram.dev") or (http.host eq "www.noogram.dev")`
- **Then:** *Dynamic* redirect → URL expression: `concat("https://noogram.org", http.request.uri.path)`
- **Status:** `301` · **Preserve query string:** ✅ on

**In zone `noogram.org`** — *Create rule* → name `www → apex`:
- **When:** expression: `(http.host eq "www.noogram.org")`
- **Then:** *Dynamic* redirect → `concat("https://noogram.org", http.request.uri.path)`
- **Status:** `301` · **Preserve query string:** ✅ on

## Step 4 — TLS / SSL (both zones)

- SSL/TLS mode → **Full (strict)**.
- **Always Use HTTPS:** on. **Automatic HTTPS Rewrites:** on.
- Edge certificate auto-issues (covers apex + `www` + `docs`). Confirm the cert lists `docs.noogram.org` before announcing docs.

---

## Verify

```sh
# .dev funnels to .org, path + query preserved, permanent
curl -sSI "https://noogram.dev/foo/bar?x=1"     | grep -iE 'HTTP/|location'
#   expect: HTTP/2 301   +   location: https://noogram.org/foo/bar?x=1
curl -sSI "https://www.noogram.dev/"            | grep -iE 'HTTP/|location'   # → 301 https://noogram.org/
curl -sSI "https://www.noogram.org/x?y=2"       | grep -iE 'HTTP/|location'   # → 301 https://noogram.org/x?y=2

# docs live (after C1 attaches the Pages custom domain)
curl -sSI "https://docs.noogram.org/"           | grep -iE 'HTTP/'            # → 200

# apex — pre-C3: whatever the placeholder serves (523/"coming soon" is honest).
#        post-C3: 200 + the install one-liner works:
curl -fsSL "https://noogram.org/cosmon/install.sh" | head -3                  # (only once C3 is live)

# mail posture
dig +short TXT noogram.dev _dmarc.noogram.dev | sort
```

## Rollback

Everything except the registrar nameserver swap (step 1) is a Cloudflare-side edit:
delete the redirect rule to stop redirecting; delete/repoint a record to change hosting.
To fully back out, restore the registrar's previous nameservers.
No content is destroyed by any step here — these are address-book edits, not data.

## Sequencing & gates

- The apex `/<tool>/install.sh` surface **stays staged** until the operator flips the
  `noogram/cosmon` repo public + cuts the first tagged release (child **C3**). Pre-flip,
  the apex 503s / "coming soon" honestly. This runbook makes the address resolve; C3 fills it.
- The docs `CNAME`/custom-domain attach is child **C1**'s move (re-point `cosmon-docs`
  from `docs.noogram.dev` to `docs.noogram.org`). Do that attach **before** deleting any
  old `.dev` docs record, so there's no doc-site outage window.
- **Do not** apply from a worktree or CI. Registrar/DNS mutation is an operator gesture.
