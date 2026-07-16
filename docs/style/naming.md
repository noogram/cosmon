# Naming rule: Noogram vs cosmon

> **Noogram = what you read about / install (the distribution).
> cosmon = what you run (the kernel, the `cs` binary).**

That one line is the whole rule. This page gives you the swap test that decides
any borderline sentence, the surface where each name is right, and the machine
that enforces the boundary so you never have to re-derive it.

Governing decision: [ADR-149](../adr/149-doc-site-topology-one-noogram-kernel-section.md)
(doc-site topology, one Noogram site, cosmon = flagship kernel section) and the
one-gate insight it records, downstream of the deliberation
`delib-20260711-8d00`. The reader-facing version of the relationship lives in
[Noogram & the Cosmon kernel](../book/src/explanation/cosmon-and-noogram.md).

## The swap test

When you are unsure which name a sentence wants, **replace the word with the
phrase "the `cs` binary"** and read it back:

- If the sentence still makes sense → it was about **cosmon** (the kernel, the
  thing you run). Write *cosmon*.
- If it now reads as nonsense → it was about the **family / the site / the
  governance / the distribution**. Write *Noogram*.

| Sentence | Swap in "the `cs` binary" | Verdict |
|----------|---------------------------|---------|
| "cosmon nucleates a molecule and tackles it." | "the `cs` binary nucleates a molecule…" ✅ | **cosmon** |
| "Install cosmon with one command." | "Install the `cs` binary with one command." ✅ | **cosmon** |
| "Noogram is the distribution for agent fleets." | "the `cs` binary is the distribution…" ❌ | **Noogram** |
| "Read the Noogram docs at docs.noogram.org." | "Read the the `cs` binary docs…" ❌ | **Noogram** |
| "Noogram composes the kernel with plugins." | "the `cs` binary composes the kernel…" ❌ | **Noogram** |

The test works because *cosmon* is a **runnable thing** (a binary, a process, a
verb) and *Noogram* is a **noun you land on** (a site, a distribution, a brand,
a commons). The `cs` binary is the smallest concrete stand-in for "the thing you
run"; if the substitution survives, you were talking about the runnable kernel.

## Which name on which surface

- **cosmon** — the kernel: the `cs` binary, its commands, its crates
  (`cosmon-core`, `cosmon-cli`, …), its lifecycle and physics vocabulary, the
  repo `noogram/cosmon`. This is the subject of most of the documentation.
- **Noogram** — the distribution: the project name, the doc site
  (`docs.noogram.org`), the apex (`noogram.org`), the external attribution
  ("built by Noogram, noogram.dev"), the governance-bearing umbrella, and the
  future plugins that compose the kernel.

The reader lands on the **Noogram** billboard; their first *action* is
**cosmon** (`cs tackle`, shown within ten seconds). The distribution is the
front door; the kernel is the engine met immediately behind it — the Ubuntu
model, not the kernel.org model.

## The one gate — why a tool earns a surface

Under [ADR-149 §4](../adr/149-doc-site-topology-one-noogram-kernel-section.md)
and [ADR-126](../adr/126-crate-frontier-two-gates.md), a tool earns a **public
surface** — a doc section **or** a `noogram.org/<tool>/install.sh` endpoint —
**iff a stranger with zero operator access can install and run it** (a public
repo plus a resolving per-platform release artifact). This is a single gate that
serves two constraints at once:

- **Anti-vaporware.** A premature tool (e.g. `topon`, with no public release
  yet) does **not** get a product/install section — only prose mention. It fails
  the gate *not-yet*.
- **Confidentiality.** The private **neurion product** (it maps operator
  infrastructure) has no public binary → it can **never** acquire a doc section
  or install endpoint. It fails the gate *never*. Only the name-scrubbed
  vendored organ crate `neurion-core` appears, as an internal crate in the API
  reference — never as a product surface.

Vendored organs (`neurion-core`, `topon-core`, `claudion`) get **reference docs
only** — they are crates inside the kernel, not products you install.

Mentioning the **neurion registry** as an integration point cosmon talks to (as
several reference pages already do) is fine — that is cosmon documenting *what it
integrates with*, not documenting the neurion *product*. The gate is about a
**product surface** (a nav title, a section heading, an install endpoint), not
about a passing prose reference.

## This rule is enforced, not remembered

You do not have to police the one gate by hand. The docs-lint
[`scripts/check-docs-one-gate.sh`](../../scripts/check-docs-one-gate.sh) (wired
into CI) fails the build if:

1. a **confidential** tool name (`neurion`, excluding the `neurion-core` organ
   crate) appears as a **nav title** in `SUMMARY.md` or a **section heading** in
   the book source — i.e. as a product surface; **or**
2. a `/<tool>/install.sh` endpoint appears in the publishable doc surface for a
   `<tool>` that is not on the installable allowlist (a public repo + release
   artifact).

The single source of the tool classification is the lint script itself; extend
the arrays there (and add a self-test canary) when a tool's status changes. See
the script header for the full contract.
