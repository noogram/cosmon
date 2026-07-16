# ─────────────────────────────────────────────────────────────────────────────
# Noogram DNS + redirect — Cloudflare (versioned source of truth)
#
# WHAT THIS IS
#   Declarative record set for the Noogram public web surface, per the
#   domain-strategy deliberation delib-20260711-8d00 (unanimous panel):
#
#     • noogram.org        — APEX. Project site + install endpoints
#                            (/<tool>/install.sh). Served by a Cloudflare
#                            Worker or Pages Function (built in child C3).
#     • docs.noogram.org   — the 33-page Diátaxis doc site (Cloudflare Pages
#                            project `cosmon-docs`, re-pointed in child C1).
#     • noogram.dev        — DEFENSIVE registration. 301 → noogram.org,
#                            path + query preserved. No independent content,
#                            ever. Docs do NOT live on .dev.
#
#   `.org` is canonical (declared in /srv/cosmon/noogram/docs/blurb.md; the
#   Debian precedent — a governance-bearing commons). Two live TLDs of one
#   brand is a newcomer footgun, so `.dev` only ever redirects.
#
# WHAT THIS IS NOT — read before you `apply`.
#   This file is PREPARED config, not an applied change. Registrar / DNS
#   mutation is an OPERATOR-RESERVED GESTURE (delib note; CLAUDE.md publication
#   gate). Do not `terraform apply` this from a worker or from CI. The operator
#   applies it by hand — either via `terraform plan/apply` with a scoped API
#   token, or via the dashboard using the record table in RUNBOOK.md.
#
#   The apex `/<tool>/install.sh` surface goes LIVE only on the operator's
#   public flip of the `noogram/cosmon` repo + first tagged release (child C3).
#   Until then the apex may 503 / "coming soon" honestly, or stay staged. This
#   DNS config is the address book, not the doorbell.
#
# PROVIDER
#   Targets the Cloudflare Terraform provider v5 (cloudflare/cloudflare ~> 5).
#   v5 renamed `cloudflare_record` → `cloudflare_dns_record` and moved redirects
#   onto the ruleset engine. `terraform plan` MUST be green before any apply.
# ─────────────────────────────────────────────────────────────────────────────

terraform {
  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 5"
    }
  }
}

# API token is supplied out-of-band by the operator (env CLOUDFLARE_API_TOKEN),
# never checked in. Scope: Zone:Read, DNS:Edit, Zone WAF/Rulesets:Edit on the
# two zones below. Nothing here reads or needs account-wide credentials.
provider "cloudflare" {}

variable "cloudflare_account_id" {
  description = "Cloudflare account that owns the two zones. Supplied at apply time; not committed."
  type        = string
}

# ─────────────────────────────────────────────────────────────────────────────
# ZONES
# Creating the zone in Cloudflare returns the two assigned nameservers; the
# operator sets those at the registrar to activate the zone. That registrar
# step is the operator gesture this config deliberately stops short of.
# ─────────────────────────────────────────────────────────────────────────────

resource "cloudflare_zone" "noogram_org" {
  account = { id = var.cloudflare_account_id }
  name    = "noogram.org"
  type    = "full"
}

resource "cloudflare_zone" "noogram_dev" {
  account = { id = var.cloudflare_account_id }
  name    = "noogram.dev"
  type    = "full"
}

# ─────────────────────────────────────────────────────────────────────────────
# ZONE noogram.org — canonical apex + docs subdomain
# ─────────────────────────────────────────────────────────────────────────────

# APEX — noogram.org
#
# The apex is claimed by the C3 install surface via a Workers Custom Domain OR
# a Pages Custom Domain. BOTH auto-create their own PROXIED apex record and will
# CONFLICT with a hand-created A/AAAA here. So we do NOT declare the live apex
# record in this file — C3's `wrangler deploy` / Pages custom-domain attach owns
# it (commented reference below).
#
# What we DO declare is a proxied placeholder so the apex resolves (and can hold
# a "coming soon" / 503) during staging, BEFORE C3 attaches its custom domain.
# `100::` is Cloudflare's documented IPv6 discard address for proxied-only
# hostnames with no real origin. C3 REPLACES this the moment it attaches its
# Worker/Pages custom domain. If C3's custom domain is already attached, DELETE
# this resource — keeping both is the conflict.
resource "cloudflare_dns_record" "org_apex_placeholder" {
  zone_id = cloudflare_zone.noogram_org.id
  name    = "noogram.org"
  type    = "AAAA"
  content = "100::" # RFC 6666 discard prefix — proxied black hole, no origin
  ttl     = 1       # 1 = "Auto" (required for proxied records)
  proxied = true
  comment = "STAGING placeholder — replaced by C3 Worker/Pages custom domain. See noogram-dns.tf."
}

# When C3 lands, the apex is instead owned by ONE of (managed by wrangler / the
# Pages custom-domain attach, not by this file):
#   • Workers Custom Domain: noogram.org → worker `noogram-apex`
#   • Pages Custom Domain:   noogram.org → project `noogram-site` (*.pages.dev)
# Cloudflare creates the proxied apex record automatically for whichever is used.

# www.noogram.org — 301 to the bare apex (canonical host is the apex).
# Proxied placeholder so the redirect rule below has traffic to act on.
resource "cloudflare_dns_record" "org_www" {
  zone_id = cloudflare_zone.noogram_org.id
  name    = "www"
  type    = "AAAA"
  content = "100::"
  ttl     = 1
  proxied = true
  comment = "www → apex 301 (see org_canonical_redirect ruleset)."
}

# docs.noogram.org — the doc site (Cloudflare Pages project `cosmon-docs`).
#
# NOTE: attaching a Custom Domain to the Pages project (child C1) auto-creates
# this exact CNAME. Declare it here ONLY if the operator provisions DNS before
# the Pages custom-domain attach; otherwise let C1's attach own it and delete
# this resource to avoid a duplicate. Kept here so the record set is complete
# and reviewable in one place.
resource "cloudflare_dns_record" "org_docs" {
  zone_id = cloudflare_zone.noogram_org.id
  name    = "docs"
  type    = "CNAME"
  content = "cosmon-docs.pages.dev" # internal Pages project name is unchanged
  ttl     = 1
  proxied = true
  comment = "docs.noogram.org → Pages project cosmon-docs. May be auto-created by C1's custom-domain attach."
}

# Anti-spoofing hardening for the apex. noogram.org sends no mail today; publish
# an explicit "no mail" posture so a defensive brand domain can't be spoofed.
# Relax later if a real mail sender is added.
resource "cloudflare_dns_record" "org_spf" {
  zone_id = cloudflare_zone.noogram_org.id
  name    = "noogram.org"
  type    = "TXT"
  content = "v=spf1 -all"
  ttl     = 1
  proxied = false
}

resource "cloudflare_dns_record" "org_dmarc" {
  zone_id = cloudflare_zone.noogram_org.id
  name    = "_dmarc"
  type    = "TXT"
  content = "v=DMARC1; p=reject; sp=reject; adkim=s; aspf=s;"
  ttl     = 1
  proxied = false
}

resource "cloudflare_dns_record" "org_null_mx" {
  zone_id  = cloudflare_zone.noogram_org.id
  name     = "noogram.org"
  type     = "MX"
  content  = "." # RFC 7505 null MX — "this domain accepts no mail"
  priority = 0
  ttl      = 1
  proxied  = false
}

# ─────────────────────────────────────────────────────────────────────────────
# ZONE noogram.dev — defensive, redirect-only
# ─────────────────────────────────────────────────────────────────────────────

# Proxied placeholders so Cloudflare's edge terminates the request and the
# redirect rule can fire. No origin server exists or is wanted.
resource "cloudflare_dns_record" "dev_apex_placeholder" {
  zone_id = cloudflare_zone.noogram_dev.id
  name    = "noogram.dev"
  type    = "AAAA"
  content = "100::"
  ttl     = 1
  proxied = true
  comment = "Redirect-only apex — proxied black hole; traffic handled by dev_redirect ruleset."
}

resource "cloudflare_dns_record" "dev_www" {
  zone_id = cloudflare_zone.noogram_dev.id
  name    = "www"
  type    = "AAAA"
  content = "100::"
  ttl     = 1
  proxied = true
  comment = "www.noogram.dev also redirects to noogram.org."
}

# Same anti-spoofing posture — a parked defensive domain is a spoofing target.
resource "cloudflare_dns_record" "dev_spf" {
  zone_id = cloudflare_zone.noogram_dev.id
  name    = "noogram.dev"
  type    = "TXT"
  content = "v=spf1 -all"
  ttl     = 1
  proxied = false
}

resource "cloudflare_dns_record" "dev_dmarc" {
  zone_id = cloudflare_zone.noogram_dev.id
  name    = "_dmarc"
  type    = "TXT"
  content = "v=DMARC1; p=reject; sp=reject; adkim=s; aspf=s;"
  ttl     = 1
  proxied = false
}

resource "cloudflare_dns_record" "dev_null_mx" {
  zone_id  = cloudflare_zone.noogram_dev.id
  name     = "noogram.dev"
  type     = "MX"
  content  = "."
  priority = 0
  ttl      = 1
  proxied  = false
}

# ─────────────────────────────────────────────────────────────────────────────
# REDIRECT RULES (ruleset engine, phase http_request_dynamic_redirect)
#
# One Single Redirect per zone. `concat("https://noogram.org", http.request.uri.path)`
# preserves the path; `preserve_query_string = true` keeps ?query intact. 301 =
# permanent (browsers + search engines cache it), which is what we want for a
# canonical-host move and a defensive-TLD funnel.
# ─────────────────────────────────────────────────────────────────────────────

# noogram.dev + www.noogram.dev  →  https://noogram.org/<same-path>?<same-query>
resource "cloudflare_ruleset" "dev_redirect" {
  zone_id = cloudflare_zone.noogram_dev.id
  name    = "noogram.dev → noogram.org (301, defensive TLD funnel)"
  kind    = "zone"
  phase   = "http_request_dynamic_redirect"

  rules = [{
    ref         = "dev_to_org_apex"
    description = "301 the whole .dev zone to the .org apex, preserving path and query."
    expression  = "(http.host eq \"noogram.dev\") or (http.host eq \"www.noogram.dev\")"
    action      = "redirect"
    enabled     = true
    action_parameters = {
      from_value = {
        status_code           = 301
        preserve_query_string = true
        target_url = {
          expression = "concat(\"https://noogram.org\", http.request.uri.path)"
        }
      }
    }
  }]
}

# www.noogram.org  →  https://noogram.org/<same-path>?<same-query>
# (Apex is the canonical host; www folds into it.)
resource "cloudflare_ruleset" "org_canonical_redirect" {
  zone_id = cloudflare_zone.noogram_org.id
  name    = "www.noogram.org → noogram.org (301, canonical host)"
  kind    = "zone"
  phase   = "http_request_dynamic_redirect"

  rules = [{
    ref         = "www_to_apex"
    description = "301 www to the bare apex, preserving path and query."
    expression  = "(http.host eq \"www.noogram.org\")"
    action      = "redirect"
    enabled     = true
    action_parameters = {
      from_value = {
        status_code           = 301
        preserve_query_string = true
        target_url = {
          expression = "concat(\"https://noogram.org\", http.request.uri.path)"
        }
      }
    }
  }]
}

# ─────────────────────────────────────────────────────────────────────────────
# OUTPUTS — the nameservers the operator sets at each registrar to activate.
# ─────────────────────────────────────────────────────────────────────────────

output "noogram_org_nameservers" {
  description = "Set these two at the noogram.org registrar to activate the zone."
  value       = cloudflare_zone.noogram_org.name_servers
}

output "noogram_dev_nameservers" {
  description = "Set these two at the noogram.dev registrar to activate the zone."
  value       = cloudflare_zone.noogram_dev.name_servers
}
