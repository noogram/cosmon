# Codeberg as a non-US sovereign mirror of GitHub remotes

> **Status:** `temp:warm` — prepared, not executed.
> **Molecule:** `task-20260614-b74b`. **Date:** 2026-06-14.
> Nothing in this note has been pushed; no organisation has been created.
> The companion script [`scripts/codeberg-mirror.sh`](../scripts/codeberg-mirror.sh)
> is a **dry-run by default** and requires `--confirm` to touch any remote.

## Why this exists (in one picture)

GitHub is a US company. Under the US **CLOUD Act**, a US authority can compel
a US provider to hand over (or, in a worst case, freeze) data regardless of
where the servers physically sit. The 2025 **CPI–Microsoft** precedent — where
a Microsoft-hosted email account tied to the International Criminal Court was
cut off after US sanctions — showed this is not theoretical: a US provider can
pull the plug on a sovereign user with no recourse.

Git is already the cheapest insurance against this. Every clone is a full,
content-addressed copy of the history — the repository is **sovereign by
construction**. What we lack today is a *warm second home* outside US
jurisdiction. With one extra remote, "GitHub is unreachable for us tomorrow"
stops being a work-stopping event and becomes a non-event: we keep pushing to
the EU mirror and carry on. That is the entire goal — turn a single point of
legal failure into a redundancy.

---

## ⚠️ Read this first — Codeberg is mission-bound to free/libre software

This is the load-bearing finding of the investigation, and it reshapes the
recommendation.

**Codeberg is a non-profit whose charter is to host *free/libre and open-source*
software. Private, proprietary repositories are explicitly out of scope.** The
concrete limits ([Codeberg docs — FAQ](https://docs.codeberg.org/getting-started/faq/),
[storage-limits blog post](https://blog.codeberg.org/new-storage-limits-on-codeberg-what-you-need-to-know.html)):

- Private repos are tolerated only as a small courtesy: **~100 MiB total per
  user**, and only for contributors to free/libre projects.
- *"Exceptions cannot be granted for proprietary software, as this is not in
  scope of their non-profit organization."*

Now look at what we would actually be mirroring. **Every cosmon/noogram repo is
private** (only `noogram/almanac` is public). cosmon, noogram, mailroom,
accord, etc. are proprietary. Mirroring them to Codeberg would:

1. blow past the 100 MiB private quota almost immediately, and
2. put proprietary content on a host that asks us not to — a values mismatch,
   not just a quota one.

**Conclusion:** Codeberg is an excellent sovereign home *for the repos we open-
source* (e.g. `almanac`, and any future public cosmon release). It is the
**wrong tool for a private proprietary backup**. For that, the same script
points at a host we control.

### The right shape for the sovereign-mirror objective

| Need | Recommended sovereign target |
|------|------------------------------|
| Mirror our **public** repos under EU jurisdiction | **Codeberg** (`codeberg.org/you`) — ideal, free, aligned |
| Mirror our **private/proprietary** repos under EU jurisdiction | **Self-hosted Forgejo** on EU infra (Hetzner/Scaleway/OVH), or a paid EU host (e.g. GitLab.com EU plan, Gitea Cloud EU). Codeberg runs Forgejo, so a self-hosted Forgejo is the *same software* without the charter constraint. |

The companion script is deliberately host-agnostic: set `CODEBERG_HOST` and
`CODEBERG_OWNER` to a self-hosted Forgejo and the *exact same* `codeberg`
remote convention and push logic apply. "Codeberg" in the remote name is then
just a label for "the EU mirror"; rename via `REMOTE_NAME=mirror` if preferred.

> **Atomic decision for the operator (one question):** for the *private*
> repos, which sovereign target?
> **1.** Self-hosted Forgejo on EU infra (recommended — full control, no charter
> limit, same software as Codeberg) · **2.** A paid EU git host · **3.** Public
> repos only to Codeberg, leave private repos GitHub-only for now (`later`).

---

## (1) Security of Codeberg — threat model and posture

**Who runs it.** [Codeberg e.V.](https://en.wikipedia.org/wiki/Codeberg) is a
German registered non-profit association (*eingetragener Verein*),
headquartered in Berlin, founded September 2018, public since January 2019. It
is funded by donations and membership — no VC, no ad model, no acquisition
exposure. It is also the umbrella that develops **Forgejo**, the Gitea fork
that powers the platform.

**Jurisdiction (the whole point).** Servers and the legal entity are in
**Germany / the EU**. Data is stored in Germany under **GDPR**. This places it
**outside the reach of the US CLOUD Act** — a US authority cannot compel
Codeberg e.V. the way it can compel a US provider. Codeberg chose the EU
specifically because members were worried that a US-hosted forge could be
struck down by bad-faith DMCA claims. This is the jurisdictional diversification
we are buying.

**Threat model — what the mirror does and does not protect against:**

| Threat | Mirror helps? |
|--------|---------------|
| GitHub account suspended / repo DMCA-struck / US legal freeze | ✅ Yes — EU copy stays reachable |
| GitHub-wide outage | ✅ Yes — push/pull continues on the mirror |
| Local disk loss | ✅ Yes — a third full copy exists |
| Codeberg outage / DDoS | ⚠️ Partial — GitHub is still up; this is *redundancy*, not a single replacement |
| Secret leakage / confidentiality breach | ❌ **No** — a second host *widens* the attack surface. Mirroring private content means trusting a second operator. Mitigate with 2FA + SSH-key-only + scrubbing secrets from history (true for GitHub too). |
| Targeted compromise of the mirror host | ❌ No — same posture as any remote; rely on 2FA and signed commits |

**Authentication / 2FA** ([docs](https://docs.codeberg.org/security/2fa/)):
- **TOTP** (RFC 6238, 30-second window) via any authenticator (Aegis, Ente, …).
- **WebAuthn / FIDO2 hardware keys** as a second factor (after TOTP is set);
  register ≥2 keys for recovery. Store scratch/recovery codes safely.
- With 2FA on, HTTP git needs a **Personal Access Token**; SSH keys work as
  usual and can themselves be backed by a WebAuthn key.
- **Recommendation:** SSH-key-only access + TOTP + a hardware key, mirroring our
  GitHub posture. The script uses `git@…` SSH URLs by design.

**Encryption at rest.** Codeberg does **not** publish a specific
encryption-at-rest guarantee for repository storage (no public statement found
as of 2026-06). Treat repo content as "protected by access control and GDPR,
not by at-rest crypto." For anything that must be encrypted at rest regardless
of host, the only robust answer is **encrypt before push** (e.g. `git-crypt` /
age-encrypted blobs) — and that protects you on GitHub too. Do not assume the
mirror encrypts; assume it does not.

**Availability / incident history.** Codeberg is a donation-funded community
forge, not an SLA-backed commercial service. It has weathered notable
**DDoS attacks (e.g. February 2025)** and has periodic outages — third-party
monitors track many short interruptions. The honest read: **good enough for a
warm backup mirror, not a primary**. Keep GitHub as primary; the mirror's job
is to exist and be current, not to be five-nines. No data-loss incident is on
record; the failures are availability blips, not durability failures.

Sources:
[Wikipedia — Codeberg](https://en.wikipedia.org/wiki/Codeberg) ·
[Codeberg docs — What is Codeberg](https://docs.codeberg.org/getting-started/what-is-codeberg/) ·
[2FA docs](https://docs.codeberg.org/security/2fa/) ·
[FAQ / limits](https://docs.codeberg.org/getting-started/faq/) ·
[storage-limits blog](https://blog.codeberg.org/new-storage-limits-on-codeberg-what-you-need-to-know.html) ·
[DDoS Feb 2025 (HN)](https://news.ycombinator.com/item?id=43053466) ·
[StatusGator — Codeberg](https://statusgator.com/services/codeberg).

## (2) Does Codeberg support organisations like GitHub?

**Yes.** Codeberg runs **Forgejo**, which has GitHub-equivalent org primitives:

- **Organisations** with members and **teams**, per-team repo permissions
  (read / write / admin), team-scoped access — see
  [Create and Manage an Organization](https://docs.codeberg.org/collaborating/create-organization/).
- **Private repositories** — supported by the software, but capped by Codeberg's
  charter (the 100 MiB private quota above). The *software* has no such limit;
  the *Codeberg instance policy* does.
- **Default limits**: **100 repos** per user **and** per organisation (liftable
  on request via [Codeberg-e.V./requests](https://codeberg.org/Codeberg-e.V./requests)).
- **Storage soft-quota**: ~750 MiB git + ~1.5 GiB packages/LFS/attachments per
  account before you must request more (public/free-software use). Resource use
  is judged "with common sense," day-to-day.
- **No pull-mirror**: Codeberg disabled *pull* mirrors (it refuses to poll other
  forges for you, for resource reasons). **Push mirroring is the supported
  pattern** — exactly what our script does (`git push` from our side). This is
  the single most important operational fact for our design.
- Extras: Forgejo Actions / Woodpecker CI, Codeberg Pages, Weblate.

So an org (`codeberg.org/you` or a dedicated `noogram` org) with teams and
private repos is fully possible *as software* — the binding constraint is the
free-software charter, not a missing feature.

Sources: [FAQ / limits](https://docs.codeberg.org/getting-started/faq/) ·
[Forgejo](https://forgejo.org/) ·
[Create an Organization](https://docs.codeberg.org/collaborating/create-organization/).

## (3) Prepared mirror setup for noogram + cosmon

### Inventory (verified 2026-06-14)

Both galaxies are single-repo, single-remote:

```
/srv/cosmon/cosmon   origin → git@github.com:noogram/cosmon.git   (private)
/srv/cosmon/noogram  origin → git@github.com:noogram/noogram.git  (private)
```

No nested git repos, no submodules. The full `you` GitHub account holds ~65
repos (via `gh repo list you`); only **`noogram/almanac` is public**, every
other is private. (Wider mirroring is a separate, later decision; this molecule
scopes only the two named galaxies.)

### Convention

- Keep `origin` = GitHub (primary, unchanged).
- Add a **second remote named `codeberg`** (or `mirror` if pointing at
  self-hosted Forgejo) → `git@<host>:<owner>/<name>.git`.
- Mirroring is **one-way, push-only** (we push; the mirror never pulls).
- Default push mode `all-tags` (push all branches + tags, **never deletes**
  remote refs). Set `MIRROR_MODE=mirror` only if you want an *exact* replica
  (force-sync that deletes remote refs absent locally) — more faithful, more
  destructive.

This composes cleanly with cosmon's `[git_remote_blocklist]` gate in
`.cosmon/config.toml`: Codeberg URLs are not blocklisted, and the gate only
blocks the internalised-substrate upstreams (`noogram/almanac`). Adding a
`codeberg` remote does **not** trip `cs done`.

### The idempotent script

[`scripts/codeberg-mirror.sh`](../scripts/codeberg-mirror.sh):

- **Dry-run by default** — prints every action, contacts nothing. `--confirm`
  arms it.
- **Idempotent remote wiring** — adds the `codeberg` remote if absent, re-points
  it if the URL changed, leaves it alone if already correct. Safe to re-run.
- **Never creates the org** — that is a manual operator gesture (below).
- **Host-agnostic** — `CODEBERG_HOST` / `CODEBERG_OWNER` / `REMOTE_NAME` /
  `MIRROR_MODE` env vars let it target Codeberg *or* a self-hosted Forgejo with
  zero code change.

```bash
# preview (default — safe, changes nothing):
scripts/codeberg-mirror.sh

# point at a self-hosted EU Forgejo instead of Codeberg, still a preview:
CODEBERG_HOST=git.noogram.eu CODEBERG_OWNER=noogram REMOTE_NAME=mirror \
  scripts/codeberg-mirror.sh

# arm it (only after the org/instance exists and operator validates):
scripts/codeberg-mirror.sh --confirm
```

### The gesture to create the Codeberg org (manual — do NOT script)

Creating the namespace is a deliberate human act (account + 2FA + charter
acceptance). It is intentionally **not** automated.

1. Create a Codeberg account at <https://codeberg.org/user/sign_up>
   (or sign in). Enable **TOTP 2FA** + register a **hardware key**; save
   recovery codes.
2. Add the SSH public key already used for GitHub:
   Settings → SSH/GPG Keys → Add Key.
3. Create the organisation: <https://codeberg.org/org/create> → e.g. `you`
   or `noogram`. (Match `CODEBERG_OWNER` in the script.)
4. **Before mirroring anything proprietary, read §"mission-bound" above.** For
   public repos, create empty repos `cosmon` / `noogram` (or let the first
   `git push --all` create them, depending on instance settings). For private
   proprietary repos, prefer a self-hosted Forgejo — point the script there.
5. Validate with a dry-run, then arm with `--confirm`.

> The Forgejo self-hosting path (recommended for private repos) is a separate
> setup — provision an EU VPS, install Forgejo, create the org there, then run
> the same script with `CODEBERG_HOST` set to your instance. That provisioning
> is out of scope for this `temp:warm` note; file a follow-up molecule if the
> operator picks option 1.

---

## What's left for the operator (nothing is armed)

1. Decide the **private-repo target** (atomic question above).
2. Create the account/org + 2FA (manual gesture).
3. Dry-run the script, eyeball the plan, then `--confirm`.
4. (Optional) add a weekly cron / LaunchAgent calling the script `--confirm` to
   keep the mirror warm. File as a separate molecule when the target is chosen.
