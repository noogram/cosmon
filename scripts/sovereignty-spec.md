# sovereignty-spec.md — the leak model the gate implements

> **Authored by the review panel of rounds 3–5** (delib-20260702-93ac /
> delib-20260704-3416 / delib-20260704-3ca3 — seats buterin *souveraineté*,
> torvalds *infra*, forgemaster *ops*, turing *auth*, janis *meta*).
> `scripts/sovereignty-gate.sh` is an **implementation** of this spec, not its
> author. The load-bearing finding of three consecutive rounds is that
> **“a gate authored by the fix it certifies trails the leak surface by one
> class”** (buterin's round-3 prophecy, landed a third time in round-5). This
> file breaks that loop: the panel writes the classes, the motifs, and the
> adversarial tokens here; the fix may only obey them. To change the gate's
> behaviour, change **this** file first, then transcribe.
>
> The tokens, patterns, and real-leak citations below are lifted **verbatim**
> from the round syntheses and per-persona responses. Provenance is cited inline
> as `[round-N / seat]`.

---

## §L — the leak: one semantic class, a new lexical shape each round

Every REFUSER in the lineage rested on the **same semantic class**: a *dangling
pointer into cosmon's private corpus* — a reference the customer (Tenant-Demo)
**cannot resolve**, because it points into a repository, a design-doc set, or a
tracker they have no access to. The class never changed; only its **lexical
shape** did, always one step ahead of the previous scrub:

| Round | Lexical shape leaked | Real token(s) | Provenance |
|-------|----------------------|---------------|------------|
| round-3 | private galaxy name as a **path** | `smithy/docs/guides/tenant-demo-operations.md` | `[round-3 / buterin]` SOUV-R3-1 |
| round-4 | internal **design-doc** citation | `ADR-132 §6 F6`, `§8j`, `§8p`, `§6 F2` | `[round-4 / buterin]` Q3 |
| round-5 | **source-tree path** + **API symbol** | `crates/cosmon-rpp-adapter/src/jwks_fetch.rs (struct TrustedIssuer)` | `[round-5 / buterin]` B-Q5 |
| round-5 | internal **tracker / bead codes** | `auth-M1`, `infra-B2`, `auth-B2`, `infra-M3`, `auth-B1`, `ops-B3` | `[round-5 / torvalds]` B-α |
| round-5 | residual bare **F-numbers** (ADR F-numbering) | `F6` in `Containerfile` | `[round-5 / synthesizer]` |

> *"This is my round-3 prophecy landing a **third** time: the B1 fix removed one
> shape of 'dangling pointer into a private corpus' (design-doc citations); the
> **sibling shape** (source-tree / API-symbol citations) sits one class over,
> un-modeled by both the deny-classes AND the allow-list."* — `[round-5 / buterin]`

## §M — why token/deny-list enumeration always trails by one class

The prior gate had two mechanisms — a **deny-list of shaped classes** and a
**WARN allow-list of unknown vocabulary**. Both are structurally blind to the
leak's composition:

> *"WARN's atom is a **lowercase alphabetic run ≥ 4 chars**. By construction it
> cannot see (a) **non-alphabetic tokens** (hex SHAs, ids, ports); (b)
> **composites of individually-legitimate tokens** (a source path is only known
> words in a path shape; a `struct Name` is two allow-listed words). The leak is
> in the *composition*, and the tokenizer destroys composition before it checks."*
> — `[round-5 / turing + buterin]`

The durable fix the panel prescribed is **not** a fourth deny-pattern:

> *"The right shape of fix is **not** a fourth deny-pattern chasing source paths
> (that trails again). It is … a **composition-aware pass** … each surfaced for
> human review the same advisory way. That closes the *class*, not the
> instance."* — `[round-5 / buterin]`

This spec sharpens that prescription into a **hard, generative rule** (§R): the
gate stops enumerating what is forbidden and instead requires every reference to
**resolve**.

---

## §R — the resolvability rule (generative, hard-DENY)

> **In the git-tracked bundle (`git ls-files dist/avatar-tenant-demo/`), every
> path-like or citation-like reference MUST RESOLVE — to a file that exists IN
> the bundle, a public URL (`http(s)://…`), or a public-standard citation
> (RFC/CVE/SPDX/license-id/…). A reference that resolves to NONE of these is a
> dangling pointer into cosmon's private corpus and is DENIED.**

This is one rule, stated positively. Its power is that it does not enumerate the
private names — it demands the customer be able to *reach* whatever is named.
The next lexical shape (source path → struct symbol → bead code → whatever
round-6 would have been) fails to resolve **by construction**, so it is caught
the moment it appears, with nothing left to keep chasing.

### §R.motifs — what counts as a "reference"

Extracted from the tracked bytes AFTER stripping `http(s)://…` URLs (a reference
inside a public URL resolves by definition). The panel's motif set:

1. **path with a file extension** — `a/b/c.ext` (e.g.
   `crates/cosmon-rpp-adapter/src/jwks_fetch.rs`).
2. **source-code file cited in prose/comment** — `*.rs` / `*.go` / `*.py` / …
   (e.g. `image_init.rs`, `jwks_fetch.rs`).
3. **repo-relative source-tree path** — rooted at `crates/` (cosmon's workspace).
4. **API symbol** — `struct`/`enum`/`trait`/`fn <CamelCase>` or a bare
   `(CamelCase)` in parentheses (e.g. `struct TrustedIssuer`).
5. **tracker / design-doc code** — `ADR-<n>`, `§<n>`, kebab bead codes
   `word-B<n>` / `word-M<n>` (e.g. `auth-B2`, `infra-M3`), uppercase codes
   `FOO-<n>` (e.g. `MA-5`).
6. **bare doc citation** — a `*.md` filename not shipped in the bundle
   (e.g. `LEAN.md`, `Operations_v1.0.md`).

### §R.resolve — what makes a reference resolve

A reference resolves iff **any** of:

- **(u) URL** — it lives inside an `http(s)://…` URL.
- **(b) bundle file** — its basename (allowing a trailing `.example` on the
  bundle side, so a reference to the concrete file a `*.example` templates
  resolves too) is a `git ls-files dist/avatar-tenant-demo/` entry.
- **(p) public standard** — it is a dashed code whose prefix is a public scheme
  (`RFC|CVE|CWE|ISO|IEC|IEEE|SPDX|ECMA|PEP|NIST|FIPS|AGPL|LGPL|GPL|MIT|BSD|MPL|EPL|Apache|CC|OCI|POSIX|Unicode|UTF|SHA|MD|CRC|OAuth|HTTP|TLS|JWT|OIDC`).
  Allow-listing PUBLIC corpora is finite and non-leaking; it does **not**
  reintroduce the enumeration trap, whose danger is enumerating what is
  *private*, not what is universally public.
- **(r) declared mount** — a path with a directory component resolves iff the
  **ENTIRE path** sits under a mount the bundle itself declares to the customer
  (the `container_path` column of `volumes-*.csv`, proven 1:1 with the compose
  mounts by the validation suite; tmpfs rows excluded — generic scratch is not a
  reader-verifiable location), or its lead component is a bundle top-level dir
  (self-reference into the recipe). **The lead component alone is NEVER
  consulted** (round-9 R1): round-8's head-only runtime-dir list accepted any
  tail behind one legitimate segment — `/tmp/noyau-vault/private/handbook.md`
  resolved `ok`, defeating the gate's own S1 forward canary with a one-segment
  prefix. A doc (`.md`) or source-code file never resolves via a mount: it is a
  citation, and a citation resolves in-bundle (b), by URL (u), or not at all.
  This is referent-resolution, not enumeration: the referent is the declared
  mount the customer can open and verify — unlike round-7's compound allow-list
  (a projection of the artifact, no referent), a fake CSV mount row cannot be
  slipped in silently because `validate-local.sh`'s `volume_parity_gate`
  requires every CSV row to have a backing `compose.yml` mount (and every
  compose mount a CSV row), fail-closed — the enforcement, not just the claim
  (round-10 B2, buterin). (Operator paths `/Users/…` are caught by
  their own DENY-class, not here.)

Anything else → **DENY**.

### §R.tests — adversarial self-test (falsifiability)

The gate embeds this exact set as a mandatory pre-scan self-test; a mismatch
aborts the gate (a gate that cannot fail is not a gate). **Every historical leak
must DENY; every legitimate reference must resolve.**

**MUST DENY — the real leaks of rounds 3/4/5:**

```
smithy/docs/guides/tenant-demo-operations.md         [round-3 / buterin]
ADR-132   §6                                        [round-4 / buterin]
crates/cosmon-rpp-adapter/src/jwks_fetch.rs         [round-5 / buterin]
crates/cosmon-rpp-adapter/src/trust_bootstrap.rs    [round-5 / buterin]
struct TrustedIssuer   (TrustedIssuer)              [round-5 / buterin]
image_init.rs   jwks_fetch.rs                       [round-5 / buterin]
auth-B2   ops-B1   infra-M3   MA-5                   [round-5 / torvalds B-α]
LEAN.md   Operations_v1.0.md                         (dangling doc pointers)
```

**MUST DENY — FORWARD canaries. SYNTHETIC shapes never seen in any prior leak.**
The round-3..6 self-test was enumerated from KNOWN leaks, so it was structurally
blind to the unenumerated shape that shipped (buterin/janis: *"a self-test
authored from the known leaks trails by one class"*). Each canary is invented,
not historical, and exercises a fix so the harness can fail on a shape that has
not yet appeared:

```
/noyau-vault/private/handbook.md   (non-mount absolute path carrying a doc file)
internal-runbooks/deploy.sh        (§R — dangling repo-internal path)
whispers/inbox/secret.json         (round-8 S1 — private-tree root, NO LONGER whitelisted)
galaxies/speck/private-strategy.md (round-8 S1 — private galaxy root, NO LONGER whitelisted)
/var/whispers/inbox/secret.json     (round-9 R1 — runtime-prefixed private tail; head-only
                                     resolution accepted it, whole-path does not)
/tmp/noyau-vault/private/handbook.md (round-9 R1 — the one-segment prefix that DEFEATED the
                                     round-8 S1 canary above; turing's falsification)
.claude/settings.json               (round-9 M3.1 — agent-substrate config path, removed
                                     from the bundle; resolves under no declared mount)
```

**MUST RESOLVE — legitimate references + the adversarial fictional tokens the
panel planted that a proper-noun scan would flag but the resolvability rule
correctly does not treat as path/citation leaks:**

```
./build.sh   validate-local.sh   forgejo/test-provision-local.sh   rpp.toml
security/trusted-issuers.toml   trusted-issuers.toml.example      (bundle)
/var/lib/gitea/custom/conf/app.ini                                 (declared mount)
target/release   Cargo.lock   forgejo-issuer.toml                  (generic/runtime)
RFC-2606   AGPL-3.0                                                 (public std)
noogram/llama-server                                               (sanctioned maker)
```

> **Adversarial proper-noun tokens** (a separate, WARN-level concern — the
> unknown-vocabulary net, not the resolvability rule). Verbatim from the rounds,
> preserved here so the WARN allow-list is never regressed: fictional galaxy
> names `veridian` `wovenreed` `belveder` `zephyrine` `tisserand` `quillhaven`
> `quorvath` `mirelgunn` `brightmoor`; operator/first names `Thibault` `Marisol`
> `delphine`; the real round-3 leak `smithy`. `[round-3 torvalds/turing;
> round-4 buterin/janis; round-5 turing/janis]` These are caught by the
> WARN unknown-vocabulary pass (proper nouns have no fixed shape); the
> resolvability rule owns the *composition* shapes instead.

---

## §R7 — round-7 sharpenings (delib-20260705-e9f4, REFUSER → IN-class fixes)

Round-6 certified clean while three parc-reachable references stood in the
bytes: the gate did not enforce its own §R. Round-7 closes four IMPLEMENTATION
defects (blockers IN the resolvability class) without changing the strategy.

- **D1 — absolute paths resolve like every reference.** `case /*) echo ok` was a
  blanket accept, broader than §R.resolve(r)'s enumerated runtime dirs. Now an
  absolute path is normalised (leading `/` stripped) and flows through the same
  lead-component dir-resolution as a relative path. See §R.resolve (r).

- **D2 — the `_vN` versioned-doc motif. ~~ADDED round-7~~ / REMOVED round-8 (S2).**
  Round-7 grew a `_vN[.N]` motif to catch `Operations_v1.0` (real name
  `Operations_v1.0.md`, whose `.0` suffix fooled the extension parse). Round-8
  removed the branch: the two tokens it guarded were already scrubbed from the
  bundle and were rewritten in prose once and for all, so the mechanism was pure
  surface with nothing to catch. `Operations_v1.0.md` (the real `.md` doc) still
  DENYs via the ordinary bare-doc rule; the naked `Operations_v1.0` is a
  proper-noun candidate for the WARN net, not a shaped reference.

- **D3 — bare compound identifiers. ~~ADDED round-7~~ / REMOVED round-8 (S2).**
  Round-7 grew a compound hard-DENY: a snake/kebab compound (≥ 2 segments)
  resolved iff present in `sovereignty-compound-allowlist.txt`, a git-tracked set
  frozen from the bundle. Round-8 removed it, for two independently-proven
  reasons (delib-20260705-d224 C3): it was **dead code** w.r.t. the scanner
  (turing — the word-split feeds the verdict only tokens containing `/` or `.`,
  so a bare compound never reached it) **and circular** (buterin/janis — its
  accept-set was a projection of the very bundle it certified: a canary that only
  fires on what was already removed). A bare identifier is not a shaped reference;
  it belongs to the advisory WARN net (mechanism 3), read by a human — the
  handful of source-symbol tokens the branch nominally guarded were rewritten in
  prose in the bundle, once and for all. The allow-list file was deleted.

  > **Why this is a subtraction, not a regression (buterin).** §R's mother rule
  > is *"a reference resolves iff it points at something the customer can reach."*
  > Membership-in-a-projection-of-the-artifact is not resolution to a referent —
  > it is the enumeration-from-the-artifact trap one altitude down. Removing it
  > returns §R to referent-resolution: the gate no longer certifies itself.

- **D4 — the gate lives in the repo, not in the travelling bundle.** The shipped
  `dist/avatar-tenant-demo/build.sh` named repo-internal literals
  (`scripts/sovereignty-gate.sh`, `.build/kernel-src`, `KERNEL_FRESHNESS_FILE`),
  disclosing cosmon's build topology. Split into **build-interne**
  (`scripts/avatar-tenant-demo-build.sh`, repo-side: invokes the gate via a repo-side
  hook before staging, stages `git archive` of the pin, bakes, pushes) and
  **build-livré** (the shipped `build.sh`, a self-contained provenance descriptor
  carrying ZERO repo-internal references). A customer with no cosmon repo cannot
  and must not build; they deploy the pushed image via compose.

> **The sibling that resolvability cannot model (carried, non-gating).** buterin's
> Q7: *disclosure ⊋ reference*. A reference can resolve perfectly and still
> *disclose* cosmon's internal mechanism, algorithm, or supply-chain relationship
> (e.g. shipping the gate's own logic). §R tests *reachability*; it structurally
> cannot test *what legible, resolving content reveals*. This is a distinct
> `temp:warm` **disclosure lens** — a semantic human review, not a gate pattern —
> tracked separately and NOT a round-7 blocker.

---

## §R8 — round-8 subtraction (delib-20260705-d224, REFUSER 6/6 → subtractive fixes)

Round-7 was the best of seven rounds, yet still 7/7: every fix that ADDED a
mechanism opened the next blocker (D1 relocated its over-acceptance into a
private-dir whitelist; D3 added a compound accept-list that was dead + circular).
The panel's decisive finding: **each additive fix trailed the leak by one class.**
Round-8's rule was therefore inverted — **every fix REMOVES mechanism, never adds
it.** The gate shrank (net-negative code lines; the 347-entry compound allow-list
deleted outright) and the two structural blockers closed by subtraction:

- **S1 — the private-dir whitelist is gone.** See §R.resolve (r). The runtime-dir
  set is now universally-public filesystem vocabulary only; a path rooted at a
  cosmon-private directory DENYs. The one legit reference that leaned on it
  resolves by referent (`oidc-identity.toml.example` now ships).

- **S2 — the compound-deny mechanism and its allow-list are gone.** See D3 above.
  A bare identifier is not a resolvability reference. §R no longer certifies
  itself from a projection of the artifact. **Honest residual (round-9 corrects
  round-8's wording):** the WARN net does NOT catch a bare compound — its
  tokenizer is alphabetic-runs-only, so `trust_bootstrap` splits into two
  allow-listed words and never surfaces. A compound of already-admitted words is
  invisible to every net. This class was always open; it is carried as a known,
  accepted residual (the human editorial pass is its only cover), not papered
  over with a safety that structurally does not exist.

- **S3 — the deploy recipe boots from its own bytes.** Non-sovereignty, but it
  gated an honest "DÉPLOYABLE": round-7's compose defaulted to phantom
  `*-local` image tags with no `.env.example`, so `docker compose up` dead-ended
  on a fresh checkout (`pull access denied`). Round-8 ships a real
  `dist/avatar-tenant-demo/.env.example` wiring every variable compose consumes, and
  the README/build.sh document the `cp .env.example .env` step. Proven by
  execution: a fresh `git archive` checkout → `cp .env.example .env` →
  `docker compose up -d --wait cosmon-server` → `/api/healthz` 200.

> **Author-inversion, finally closed (buterin, three rounds running).** §R7 D3
> had been *rewritten to bless the weaker enumerative code* rather than the code
> raised to referent-resolution. Round-8 did not re-raise the spec to match a
> mechanism — it deleted the mechanism, so the spec and the code agree at §R's
> mother rule again. The gate is smaller than before round-7.

---

## §R9 — round-9 whole-path resolution (delib-20260706-2042, B1)

Round-8's S1 removed the private *names* from the runtime-dir list but left the
*head-only mechanism* that made any name dangerous: the path-with-directory
branch inspected only the leading component (`lead ∈ runtime-dirs ∪ bundle-top →
ok`, tail never read). turing's falsification: `/var/whispers/inbox/secret.json`
and `/tmp/noyau-vault/private/handbook.md` both resolved `ok` — the second
defeats the gate's own S1 forward canary with a one-segment `/tmp/` prefix. The
private-corpus *class* was not denied; only the shapes where the private root
is the head.

- **R1 — the runtime-dir list is deleted; the whole path resolves or nothing
  does.** See §R.resolve (r). A dir-path resolves only as bundle self-reference
  or when the ENTIRE path sits under a mount the bundle's own `volumes-*.csv`
  declares (tmpfs excluded). Doc/source files never resolve via a mount. This is
  a subtraction (one more accept-list gone — the last enumerated vocabulary in
  the resolver) and the accept-referent moved from a gate-internal constant to a
  customer-facing contract artifact the suite already proves 1:1 with compose.

---

## §R10 — round-12 polarity inversion: DENY-BY-DEFAULT (delib-20260707-8eca)

Rounds 3–11 all failed the same way. The resolver's positive arm **allowed by
default**: a path short-circuited to `ok` the moment its *lead segment* was on a
trusted-lead whitelist (`RESOLV_ABS_ROOTS` for absolute, `RESOLV_REL_ROOTS` for
relative), **without ever requiring the whole path to resolve to a customer-
reachable referent**. Any private form whose lead was a trusted root — `/opt/…`,
`/etc/…`, `/home/…`, `state/…`, `noogram/…` — passed, because the tail was never
consulted. Adding denials (motif, secret-ext, secret-basename) never closed the
class: an accept-set defined by *subtracting finite enumerations from an open
lead-whitelist* trails the leak surface by one lexical class, for ever. Eleven
rounds proved it. **A defect that survives its own named fix is a CLASS defect.**

The fix, done **once**, inverts the polarity. **The enumeration now names the
small, finite set of AUTHORISED referents, not the infinite set of forbidden
private names.**

- **I1 — the two trusted-lead whitelists are DELETED** (`RESOLV_ABS_ROOTS`,
  `RESOLV_REL_ROOTS`) along with their lead-segment short-circuits. The absolute
  and relative arms are **unified**: after normalisation a path with a directory
  component flows through ONE predicate.

- **I2 — a path-with-directory resolves `ok` ONLY IF it resolves POSITIVELY** to
  one of: **(a)** a declared mount (`container_path` in `volumes-*.csv`, held 1:1
  with compose by `volume_parity_gate`); **(b)** a file that exists in the bundle
  (`git ls-files`, matched by basename / bundle-top self-reference); **(c)** a
  reference whose **host is a documented public / reader-reachable domain**
  (`RESOLV_PUBLIC_HOST`, §R10.host — an explicit `http(s)://` scheme also
  qualifies, but the scanner strips whole URLs so a scheme never survives into a
  token; a bare dot in a segment is **never** enough); **(d)** an entry of the
  **positive whole-path ALLOW** below — matched **EXACTLY**, no open prefix, no
  trusted lead. **Otherwise → DENY.** A `..` segment never resolves (it escapes
  any referent).

- **I3 — the denial lists become REDUNDANT** and are demoted to commented
  defense-in-depth. With I2, a secret resolves positively nowhere → it denies by
  construction, whether or not any list names it. `RESOLV_PRIVATE_MOTIF`,
  `RESOLV_SECRET_EXTS`, `RESOLV_SECRET_BASENAMES` are kept only as cheap EARLY
  denials (they can add a deny, never grant an accept); they are **no longer the
  closure**.

**Closure criterion (the generational test becomes unwinnable).** Because every
non-referenced path denies by construction, NO invented private form can pass —
`/opt/client-financials`, `state/tenant-secrets`, `noogram/client-roster`,
`/opt/keys/master.age`, `usr/local/bin/exfil-tenant-db` all resolve positively
nowhere. This is *per-class* closure, not the *per-instance* closure of rounds
3–11. The self-test wires these generative forms as MUST-DENY so closure is
proven every run; but the proof is structural, not enumerative — the next unknown
private name is denied for the same reason, without ever being listed.

### §R10.allow — the positive whole-path ALLOW (the panel-owned accept-referent)

Per buterin's freeze-set governance finding (round-11): the accept-referent must
be a **panel-owned contract**, not "whatever the gate author froze in" — exactly
as the mount arm was externalised to the CSV. This is that contract. Each entry
is a COMPLETE normalized path the 19-file bundle actually ships; the gate's
`resolves_path_allow` transcribes it. A NEW legitimate reference is added **here
first**, with a one-line justification, then to the gate — never the reverse.

| Class | Entries (exact, case-folded) |
|-------|------------------------------|
| Prose word-pairs | `401/403` `erofs/eacces` `http/https` `above/below` `application/json` `docker/dockerfile` `fonts/cdn` `idp-ext/idp-int` `linux/amd64` `linux/arm64` `models/user` `reboot/redeploy` `reserved/invalid` `sign-up/creation` `stdout/stderr` `suites/legs` `uid/gid` `unreachable/healthz-blind` `volume/tmpfs` `volumes/tmpfs` `target/release` `cosmon-issuer-handoff/v1` `state/galaxies` `.example/git` |
| Documented FS / prose referents | `tenant-demo/cosmon-server` `.cosmon-provision/admin-pass` `internal/underivable` `internal/request-derived` `credentials/iam` `handoff/forgejo-issuer.toml` |
| Sanctioned maker image | `noogram/llama-server` |
| URL path segments (post URL-strip) | `api/healthz` `api/v1/user` `api/v1/user/applications/oauth2` `v1/auth/me` `v1/molecules` `login/oauth/keys` `.well-known/openid-configuration` |
| OS device nodes (universal, finite) | `dev/null` `dev/zero` `dev/full` `dev/random` `dev/urandom` `dev/stdin` `dev/stdout` `dev/stderr` `dev/tty` `dev/console` |
| Cited OS / container binary paths | `bin/sh` `usr/bin/env` `usr/bin/dumb-init` `usr/sbin/nologin` `usr/local/bin` `usr/local/bin/cs` `usr/local/bin/cs-oidc-mock` `usr/local/bin/cs-rpp-adapter` `usr/local/bin/cosmon-rpp-adapter` `kernel-bin/cs` `kernel-bin/cs-oidc-mock` `kernel-bin/cosmon-rpp-adapter` |
| Cited package cache | `var/lib/apt/lists` |
| Recipe HOME / build / state dirs | `cosmon/.config` `cosmon/.config/cosmon` `cosmon/.cosmon` `build/cosmon` `.build/kernel-src` `.cosmon/state` `tmp/gitea` |
| Script-syntax fragments (round-13 §R13; NOT filesystem paths — the bundle's own validation/provision scripts embed these sed/regex/query fragments; enumerated EXACTLY so a generative private form matches none) | `s/^` `\1/p` `api/v1/users/search?limit` |

> **Why exact match, not an anchored prefix.** An open prefix (`usr/local/bin/*`)
> re-admits an arbitrary tail (`usr/local/bin/exfil-tenant-db`) and a
> `..`-traversal — the same hole the lead-whitelists had, one altitude down. Exact
> match IS the closure: a path one character different from an authorised referent
> denies. Binary/config paths whose leaf is a shipped bundle file
> (`…/entrypoint.sh`, `…/provision.sh`) resolve earlier by basename and never
> reach this set — so the enumeration stays minimal. The set is deliberately
> brittle: a new binary or endpoint surfaces for human review, exactly as the WARN
> vocabulary freeze admits a new word.

### §R10.host — the documented public-host set (the URL/host accept-referent)

The dotted-hostname referent of §R.resolve(c) is governed exactly like the mount
CSV and the §R10.allow set: it is a **small, finite, panel-owned enumeration of
documented public / reader-reachable hosts**, NOT the heuristic "any label with a
dot is a URL". Each host is one the customer can actually reach; a host-shaped
token resolves iff its host segment (everything before the first `/`, case-folded)
is a member. A NEW host is added **here first**, with a one-line justification.

| Host | Why it is reader-reachable |
|------|----------------------------|
| `codeberg.org` | public Forgejo registry — the base image origin (`manifest.toml`, `forgejo/Containerfile`) |
| `registry.vendor.tenant-demo.io` | the customer's OWN vendor image registry, documented in the bundle (`manifest.toml`, `.env.example`, `compose.yml`) — reachable by the recipient by construction |

### §R10.bare — the positive BARE-token ALLOW (round-14, delib-20260708-d5a4)

The single-segment sibling of §R10.allow. A token with **no directory component** —
a bare filename (`app.ini`), a single-segment path normalised from a leading slash
(`/tmp` → `tmp`), a bare dir mention (`nucleons/`), or a dotted config/jq accessor
(`sibling.llama`) — resolves `ok` ONLY IF it is an EXACT member of this set, a
shipped bundle file (`BUNDLE_BN`, resolved earlier), or a numeric-dotted version /
IP literal (§R.resolve(v), below). Everything else → DENY. This closes the two
round-13 residual arms (bare-filename `*)` and the bare-dir short-circuit) that
returned a blanket `ok`. Same governance as §R10.allow: **panel-owned, added here
first, EXACT match** — a token one character off DENYs, so no invented private bare
form (`cap-table`, `tenant-secrets`, `master.age`) can ride an authorised one.

| Class | Entries (exact, case-folded) |
|-------|------------------------------|
| Legit non-bundle filenames | `cargo.lock` `app.ini` `forgejo-issuer.toml` `state.json` `err.log` `nuc.log` `docker-entrypoint.sh` `docker-setup.sh` |
| Single-segment OS / build / state / container dirs cited bare | `tmp` `cosmon` `handoff` `build` `forgejo` `kernel-bin` `git` `admin-pass` `healthz` `nucleons` `.build` `.cosmon-provision` `.git` `volumes-` |
| Config-key / TOML-section / git-config / template / jq accessors (dotted, but a KEY, not a path) | `org.opencontainers.image.title` `org.opencontainers.image.authors` `org.opencontainers.image.url` `org.opencontainers.image.licenses` `org.opencontainers.image.version` `org.opencontainers.image.revision` `org.opencontainers.image.description` `sibling.llama` `sibling.forgejo` `logging.driver` `user.email` `user.name` `trust_bootstrap.issuer` `.state.health.status` `.server.arch` `.architecture` `.client_id` `.id` `.name` `.iss` `.issuer` `.is_admin` `.data` `.d` `.so` `.csv` `.toml` `.example` `.build-markers` `.val-seed-` `.gitignore` `.env` |
| Non-public-TLD local email literals the provision scripts embed | `@noreply.localhost` `cosmon@tenant-alpha.local` |
| Bare script-syntax fragments (sed address / shell param-expansion residue; NOT references) | `^services` `^volumes` `^-` `ext_root_url%` `p` `3` `s` `e.g` `i.e` `.tmp.$$` |

> **§R.resolve(v) — the numeric-dotted VERSION / IP shape.** A token matching
> `^v?[0-9]{1,3}(\.[0-9]+)+(-[A-Za-z0-9]+)*$` (optional `v`, a ≤3-digit first group
> then ≥1 more dot-separated numeric groups, optional `-alnum` build suffixes) is a
> POSITIVE, non-leaking referent: a
> private-corpus name never has an all-numeric dotted core. It covers `v3.0`,
> `v3.0-amd64`, `2.5.0`, `1.88-bookworm`, `0.0.0.0`, `127.0.0.1`. This is a SHAPE
> class, not an enumeration, so version bumps do not churn the allow-set — but it is
> tight: one letter out of place (`cap-table.xlsx`, `master.age`) fails it and DENYs.

> **§R.resolve(z) — the zero-alphanumeric residue guard.** A token carrying no
> `[A-Za-z0-9]` at all (`/`, `//`, `/^`, `$/`, `/#`) is sed/shell-syntax residue the
> word-split emits; it names nothing, so it can carry no reference → `ok`. This is
> NOT the deleted Door A (which waved through slashed tokens that DID carry an
> alphanumeric private tail): it fires ONLY when the WHOLE token is punctuation.

## §R13 — round-13 closure: the two heuristic accept-doors (delib-20260707-3b7e)

Round-12 inverted the resolver to DENY-BY-DEFAULT (§R10) and that inversion holds
— the terminal `:457` deny-by-default is proven, the demoted denial lists proven
redundant. But the re-review (REFUSER 12/12) found the inversion left **two
pre-existing heuristic `ok` short-circuits upstream of the `:457` fall-through**,
so the *generational* guarantee ("any invented private form denies by
construction") was still false one door over. Round-13 closes both. **The rule is
now: every `ok` return of the resolver is backed by a POSITIVE, exactly-enumerated
referent; the single unmatched fall-through is `:457 = deny`.**

- **Door A — the non-path-char → prose exemption.** A token carrying a char
  outside `[A-Za-z0-9._/-]` was blanket-accepted as "a sed/regex/prose fragment,
  not a filesystem reference". But `~/tenant-demo-secrets/cap-table` is an ordinary
  home-relative path whose only non-path char is `~` — it was waved through with a
  private tail. **Any** private form dressed with one exotic char (`~ % # &`)
  escaped the same way: an open class. **Fix: the prose exemption no longer applies
  to a token that has a path separator (`/`).** Once a token contains a `/` it is a
  path and MUST resolve to a positive referent or DENY. The prose exemption
  survives only for the slash-free case (a bare word — §R8 S2). The three genuine
  script-syntax fragments the bundle's own scripts embed (`s/^`, `\1/p`,
  `api/v1/users/search?limit`) are enumerated EXACTLY in §R10.allow — not exempted
  by shape. A generative private form matches none of them → denies.

- **Door B — the dotted-hostname → public-URL exemption.** A token whose lead
  segment contained a dot (`^[a-z0-9-]+(\.[a-z0-9-]+)+/`) was accepted as "a public
  registry / URL reference". But that cannot tell `codeberg.org/forgejo/forgejo`
  (public, reachable) from `vault.tenant-demo-internal/master-key`, `internal.corp/tenant-secrets`,
  `whispers.backup/inbox`, `noyau-vault.io/dump` (private, unreachable) — the exact
  §R failure mode, and itself a lead-trust. **Fix: a host-shaped token resolves iff
  its host is a member of the documented public-host set (§R10.host).** A bare dot
  in a segment is never enough. (An explicit `http(s)://` scheme also qualifies by
  §R.resolve(u), but the scanner strips whole URLs, so no scheme survives into a
  token — the whitelist is the operative test.)

**Closure enumeration (the round-13 structural proof).** After both fixes, every
`ok`-returning site of `resolv_token_verdict` is adossé to a positive referent:
bundle file (`BUNDLE_BN` / `BUNDLE_TOP`), declared mount (`resolves_mount`, CSV),
documented public host (§R10.host), the exact whole-path ALLOW (§R10.allow,
including the three script-syntax fragments), a public-standard dashed prefix
(§R.resolve(p)), or a slash-free bare word / single-segment dir mention
(prose — §R8 S2). **No heuristic `ok` remains.** The only unmatched fall-through in
the has-directory arm is `:457 = deny`. A private form invented without an
`http(s)://` scheme and without a positive referent denies by construction — this
is the class closure the eleven-round leak-whitelist could never reach.

## §R14 — round-14 closure: the two residual bare-token arms (delib-20260708-d5a4)

Round-13's closure enumeration (§R13) was **scoped to the has-directory (`*/*)`)
arm**. The re-review (REFUSER 13/13) found the structural `ok`-enumeration surfaced
**two residual heuristic `ok` terminals in the sibling arms** that no round-13 change
touched — the same species as Doors A/B (a referent-less private token receiving
`ok`), one arm over:

- **The bare single-segment DIR short-circuit** (`case "${norm%%+(/)}" in */*) : ;;
  *) RTV=ok`) fired *before* the private-motif net, so `client-roster/`,
  `tenant-secrets/`, `vault/`, `secrets/`, `cap-table/` all → `ok`. The bare-dir case
  was **fully silent** (no DENY, no WARN — its word-parts pre-frozen in the allow-list).
- **The bare-FILENAME arm** (`*)`, whose only deny was `[ "$ext" = md ]`) returned a
  blanket `ok` for every other extension, so `cap-table.xlsx`, `credentials.json`,
  `tenant-secrets.env`, `payroll.db`, `master.age`, `master-key.txt`, `whispers.db`,
  `id_ed25519.bak` all → `ok`. Every deny list (secret-ext, secret-basename) is finite,
  so a novel private extension escaped by construction.

**The fix, both arms, mirrors §R10: DENY-BY-DEFAULT.**

- **The bare-dir short-circuit is DELETED.** A bare dir mention now flows through the
  same deny-by-default terminal as every path: `ok` only via `resolves_mount`,
  §R10.allow, or §R10.bare. `nucleons/` survives via its exact §R10.bare entry;
  `client-roster/` and the rest resolve positively nowhere → deny.
- **The bare-filename arm resolves `ok` ONLY IF** the token is a shipped bundle file
  (`BUNDLE_BN`), a numeric-dotted version/IP shape (§R.resolve(v)), zero-alphanumeric
  residue (§R.resolve(z)), or an EXACT §R10.bare referent. Otherwise → DENY.

**Closure enumeration — EVERY `RTV=ok` site of `resolv_token_verdict`, each adossé to
a positive referent (the operator's decisive test; ZERO heuristic `ok` remains):**

| # | Site | Referent backing the `ok` |
|---|------|---------------------------|
| 1 | empty-after-trim (`[ -z "$tok" ]`) | safe base case — names nothing (turing's §R13 :335 ruling) |
| 2 | public-standard dashed prefix | `RESOLV_PUBLIC_PREFIX` (RFC/CVE/SPDX/…) |
| 3 | prose word (no `/`, no `.`) | **prose-sans-slash** — a bare word is not a shaped reference (§R8 S2) |
| 4 | zero-alphanumeric guard (round-14) | §R.resolve(z) — punctuation residue names nothing |
| 5 | `BUNDLE_BN[$base]` | shipped bundle **file** |
| 6 | `BUNDLE_TOP[$lead]` | bundle **top-level** self-reference |
| 7 | `resolves_mount` | declared **mount** (`volumes-*.csv`, held 1:1 by `volume_parity_gate`) |
| 8 | `RESOLV_PUBLIC_HOST` | documented **public host** (§R10.host) |
| 9 | `resolves_path_allow` | exact **§R10.allow** whole-path |
| 10 | `resolves_bare_allow` (has-dir terminal) | exact **§R10.bare** (single-segment dir) |
| 11 | empty base (round-14) | §R.resolve(z) — `/`, `//` name nothing |
| 12 | `is_version_shape` (round-14) | **§R.resolve(v)** numeric-dotted version/IP |
| 13 | `resolves_bare_allow` (bare-file arm, round-14) | exact **§R10.bare** |

Sites 1, 4, 11 accept **non-references** (they name nothing — no leak surface). Sites
2, 3, 12 accept **provably-public shapes** (standard code, bare prose word, version
number — none can encode a private corpus path). Sites 5–10, 13 accept **exact
positive referents** the reader can reach. **No site accepts a shaped private
reference.** The only unmatched fall-through in EITHER arm is `deny`.

**Governance (janis's structural remedy, wired this round).** A per-terminal
REFERENT-BACKING self-test (`referent_backing_self_test`) asserts, for a representative
token at each referent-backed `ok`-site, that a MINIMALLY-mutated sibling with the
referent removed flips to `deny`. A referent-less `ok` terminal (the round-13 residual)
is UNWRITEABLE under this test — there is no referent to remove, so the pairing cannot
be authored. This is the signal that breaks the streak *before* merge, not after.

> **Known residual (non-blocking, carried to the round-14 re-review).** The §R10.bare
> accept-set is larger than round-13 estimated (~50 entries vs the "~5" the outcomes
> doc projected): single-segment OS/build paths (`/tmp`, `/cosmon`), OCI label keys,
> and jq/template accessors all route through the bare arm and must stay `ok` to hold
> **0 over-denial** on the 19-file bundle. This is the accepted deny-by-default
> tradeoff (buterin: "spec-first, add with a one-line justification") — a NEW legit
> prose/config token surfaces for a §R10.bare entry, exactly as the WARN freeze admits
> a new word. A **spec↔gate parity test** (parse §R10.bare from this file, diff against
> `resolves_bare_allow`, fail-closed) is the recommended next hardening — filed
> `temp:warm`, not blocking this closure.

---

## §P — buterin's full independent pattern set (round-3, verbatim)

Preserved so the shaped DENY-classes are never silently narrowed. `[round-3 / buterin §2]`

| # | Class | Pattern |
|---|-------|---------|
| 1 | Molecule ids | `(task\|delib\|idea\|issue\|signal\|spark\|decision\|mol)-\d{8}-[0-9a-f]{4}` |
| 2 | Branches/worktrees | `feat/`, `worktree` |
| 3 | Tailnet | `\.ts\.net`, `tailscale`, `tailnet` |
| 4 | Operator identity | `you`, `you`, `s[ée]rie` |
| 4b | Internal tenant fixture | `democorp` (reclassified out of class 4 on 2026-07-16 — a fictional placeholder, not an operator identity) |
| 5 | Absolute operator paths | `/Users/...` |
| 6 | Emails / gmail | `[\w.%+-]+@[\w.-]+\.\w+`, `gmail` |
| 7 | Private galaxy names | `smithy`, `mailroom`, `skylight`, `accord`, `speck`, `almanac`, `showroom`, `chancery`, `agora`, `souffleur`, `lumen` |
| 8 | Other-avatar ids | `avatar-jordan`, `avatar-[a-z]+` |
| 9 | Private domains | `serie""\.dev`, `\.eth\.limo` |
| 10 | Internal-cadence dates | `2026-0[4-7]-\d\d`, `20260[4-7]\d\d` |
| 11 | Git/build residue in tree | `.git`, `.build`, `.DS_Store` (as actual files) |
| 12 | base64/hex blobs | `[A-Za-z0-9+/]{24,}={0,2}` |
| 13 | IPv4 | `(\d{1,3}\.){3}\d{1,3}` |
| 14 | Internal tools | `neurion`, `archive-service`, `zotero` |
| 15 | Maker attribution | presence of `Noogram`, absence of anything private |

Classes 1–14 (minus the design-doc citation class, now subsumed by §R) live in
the gate's DENY-class mechanism; class 15 is satisfied by the external
attribution rule (maker = `Noogram`). The former `ADR-[0-9]+|§[0-9]` DENY class
was **removed** — §R subsumes it generatively.

## §D — discharged, non-blocking (kept for the record)

- **Kernel commit SHA** — a class neither mechanism covers, *but* opaque, inert
  without private-repo access, and the deliberate minimal-disclosure provenance
  primitive. `[round-5 / buterin]` Recommendation: model it as an
  explicitly-allowed known literal so a *future stray* hash surfaces. Provenance
  in the bundle is cited **by kernel SHA only**.
- **Residual len<4 / leet WARN holes** — narrower class, currently unpopulated by
  any known private literal. `[round-5 / turing]` `temp:warm`.
