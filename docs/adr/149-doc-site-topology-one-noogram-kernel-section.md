# ADR-149 — Doc-site topology: one Noogram site, cosmon as the flagship kernel section

**Status:** accepted
**Date:** 2026-07-11
**Decider:** Noogram
**Authoring task:** `task-20260711-590c` (🔧 task) — the **C1** child of the
doc-topology/domain deliberation.
**Parent deliberation:**
`delib-20260711-8d00`
(panel: wheeler · godin · jobs · torvalds · karpathy) — unanimous on **topology A**
across all five seats; no substitution hypothesis fired.
**Relates:** ADR-112 (noogram
subdomain pattern for verticales), [ADR-132](132-kernel-plugin-catalog-ecosystem.md)
(kernel/plugin catalog), [ADR-133](133-one-repo-artifact-map-membrane.md) and
[ADR-126](126-crate-frontier-two-gates.md) (the public/private remote split that
keeps the neurion product out of any public surface).

---

## 1. Context

The documentation was rebuilt as a 33-page [Diátaxis](https://diataxis.fr/) mdBook
titled **Noogram** (`docs/book/`, `book.toml`). Two open questions remained:

1. **Domain.** Publish where — `docs.noogram.dev`? an isolated `cosmon.dev`? an
   umbrella with per-tool sub-sites?
2. **Structure.** Is this a cosmon site, a Noogram site, or a family of sites?

The background fact the decision turns on: the four names once shown as "The
Particle Ecosystem" (cosmon / neurion / topon / claudion) are **not four peer
products**. cosmon is the kernel; `neurion-core`, `topon-core`, and `claudion`
are crates **vendored inside** it; the standalone neurion/topon MCP servers are a
**future** distribution plugin; the full neurion product is **confidential**
(it maps operator infrastructure). The upstream investigation
`task-20260711-82de/ecosystem-positioning.md` §3–4 flagged the peer-product table
as the one real doc bug.

## 2. Decision

**One site. cosmon is the flagship kernel section. Other tools become sections —
never sub-sites — only when they ship.** (Topology A.)

- **Domain (docs).** The canonical doc host is **`docs.noogram.org`**. This
  supersedes the earlier same-day `docs.noogram.dev` choice recorded in
  `wrangler.toml`/`WEBDOCS.md`. `noogram.dev` is registered defensively and
  301-redirects to `noogram.org`; docs do **not** move to `.dev`. Two live TLDs
  of one name is a newcomer footgun ("which one is real?"); the developer/public
  split rides the `docs.` subdomain, not a TLD.
- **Structure.** The single Noogram site is the front door. cosmon is the first,
  most prominent section; the Getting-Started path shows `cs` within ten seconds.
  No per-tool sub-domains, no per-tool sub-sites. When a peripheral tool ships a
  public binary, it earns a **section** integrated into the same Diátaxis spine.
- **README framing.** The peer-product table is retired in favour of the
  two-layer framing already in `README.md` ("Cosmon is a kernel, not a bundle"):
  **kernel = cosmon (`cs`); organs = vendored crates; distribution plugins =
  future.**

The apex sentence on the landing page:

> **Noogram runs ten AI agents in parallel without losing track of who is doing
> what — a distribution for agent fleets, built on the cosmon kernel.**

## 3. Why not the alternatives

- **Isolated `cosmon.dev` (option B).** Copies the *form* of the
  kernel.org/debian.org split without the *substance* — that split exists because
  they are separate organisations with independent maintainers and cadences. One
  team, one org, one cadence: a second domain is an `if`-branch kept in sync
  forever and a second front door a newcomer must reconcile.
- **Per-tool sub-sites (option C).** Every sub-domain is a promise; a sub-domain
  for a tool with no public repo is vaporware with a URL — *and* it builds the
  empty shelf onto which the confidential neurion product could later be
  accidentally placed.

The Ubuntu model wins: newcomers visit ubuntu.com, never kernel.org — the
distribution is the human front door, the kernel is the engine. But the reader's
*first action* is `cs` (cosmon), so the kernel is met within ten seconds and the
model teaches itself.

## 4. The one gate (the load-bearing insight)

A public surface — a doc section **or** a `noogram.org/<tool>/install.sh`
endpoint — exists **iff a stranger with zero operator access can install and run
the tool** (public repo + resolving per-platform release artifact). One rule
serves both the anti-vaporware constraint and the confidentiality guardrail:

- Vendored organs (`neurion-core`, `topon-core`, `claudion`) get **reference docs
  only**, never a product/install surface.
- The private neurion product has no public binary → it can **never** acquire a
  section or install endpoint. It is structurally invisible, not invisible by
  memory. Enforced by the `noogram/cosmon-private` ↔ `noogram/cosmon` remote split
  (ADR-126 / ADR-133).

Codifying this gate as a mechanical docs-lint is **C4's** job
(`delib-20260711-8d00`), downstream of this ADR.

## 5. Scope boundary

This decision (C1) covers the **doc-site topology and its hosting target**. It
does **not** register domains, provision the `noogram.org` zone, wire the
`noogram.dev → noogram.org` 301, or migrate the project's `noogram.dev`
attribution strings — those are **C2** (domain/DNS). The `wrangler.toml`
publication gate is unchanged: `docs.noogram.org` goes live only on the operator's
public-flip gesture behind the membrane-repair confidentiality scrub
(`task-20260616-cc22`), never on push.

## 6. Consequences

- `docs.noogram.org` is the single canonical doc host; the `.dev` variants and the
  earlier cosmon-specific hosts are retired in the doc-site config.
- New peripheral tools integrate as sections of this site, gated on
  stranger-installability; contributors never spin up a sub-domain.
- The README sells one kernel + vendored organs + future plugins, not four peers.
- C2 inherits the apex/DNS/301 work; C4 inherits the mechanical one-gate lint.
