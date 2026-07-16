# NPL-1.0 — Validation Plan — REJECTED

> **STATUS: REJECTED, 2026-05-09 (ADR-092).**
>
> The bascule landed earlier than Day J and on a different licence:
> AGPL-3.0-only at the core, Apache-2.0 at the frontier, no custom
> licence. The seven-step validation plan below is preserved for the
> record but is no longer in flight; €400–€800 of IP counsel review,
> peer review, and OSI submission are all spared by adopting the
> FSF-canonical AGPL-3.0 directly.
>
> See [`../../adr/092-license-bascule-mpl-to-agpl.md`](../../adr/092-license-bascule-mpl-to-agpl.md).

---

# NPL-1.0 — Validation Plan

**Status:** INTERNAL. Draft companion to `NPL-1.0-draft-v0.1.md`.
**Owner:** pilot (Noogram).
**Target:** bascule repo MPL-2.0 → NPL-1.0 at "Day J" (mid-October 2026).

This document orders the steps required to move NPL-1.0 from a self-authored
draft to a legally reviewed license adopted by the cosmon repository, and
(optionally) republished as a reusable standalone license.

---

## T+0 — Anchor state

- Repo currently under MPL-2.0 (transitional, task-20260415-100f).
- `NPL-1.0-draft-v0.1.md` complete in `docs/license/`.
- Design validated by delib-20260415-6e50 (economic panel, 11/11 on the
  §K4/§K5/§K6 trio), delib-20260415-89dc (C1: no proprietary blockchain),
  delib-20260415-e2f0 (verdict B: offline-first binary), delib-20260415-8fc0
  (no smart-contract enforcement required; contract + trademark + verifier
  suffices).

---

## Step 1 — Self-review (T+0 → T+7)

**Scope:** pilot-only, no external party.

- Re-read draft v0.1 line by line against the four delibs listed above.
  Produce a cross-reference table (one row per §K clause × delib ruling).
- **Coherence test — "smart contract unneeded":** verify that §K4
  attestation persistence is enforceable through (a) license contract law,
  (b) the `Noogram` trademark (INPI n°5248264), and (c) a future verifier
  formula — and does *not* require on-chain enforcement. Cohérence with
  delib-8fc0.
- **Coherence test — offline-first:** §K6 carve-out must be wide enough
  that a solo operator running cosmon locally triggers no NPL obligation.
  Cohérence with delib-e2f0 verdict B.
- **Coherence test — no proprietary blockchain:** §K4 persistence language
  is substrate-neutral (filesystem, git, IPFS, any DAG store). Cohérence
  with delib-89dc C1.
- **Output:** annotated draft v0.1 + self-review notes in
  `docs/license/review-notes-v0.1.md` (to be produced during step 1 run).

**Gate to Step 2:** all four coherence tests pass, no self-identified
ambiguity remaining in §K4/§K5/§K6.

---

## Step 2 — IP counsel review, FR + CH jurisdictions (T+7 → T+30)

**Scope:** one paid consultation with an IP lawyer specialized in software
licensing, with coverage of both French and Swiss jurisdictions (the two
jurisdictions closest to Noogram operations).

- **Sourcing:** APRAM annuaire (French IP lawyer directory) for initial
  shortlist; cross-reference with Swiss recommendations.
- **Budget:** ~€400–€800 for a 2–3 hour consultation + written opinion.
- **Deliverable requested from counsel:** written memo answering:
  1. Is §K4's *format vs. content* distinction legally cognizable in FR/CH
     copyright law? Can a court meaningfully enforce the format obligation
     without capturing the content?
  2. Does §K5's "symbolic €1" disclosure hold as a genuine licensing
     signal, or will it be re-qualified as a paywall/licensing fee?
  3. Is §K6's private-use carve-out unambiguous enough to survive
     adversarial reading?
  4. Strategic call: should NPL-1.0 be submitted to OSI for approval, or
     is it preferable to remain a "source-available copyleft" without
     OSI label?
- **Output:** draft v0.2 incorporating counsel's remarks; one-page
  summary of legal opinion checked into `docs/license/` (with counsel's
  permission) or kept in private archive.

**Gate to Step 3:** counsel issues no blocking objection; any objections
are resolved in v0.2.

---

## Step 3 — Peer review by OSS licensing experts (T+30 → T+45)

**Scope:** private solicitation of 2–3 OSS licensing specialists.

- **Candidate reviewers:** Bradley M. Kuhn (SFLC / Software Freedom
  Conservancy), Heather Meeker (OSS licensing practitioner), and one
  additional reviewer to be selected during step 2.
- **Channel:** cold email (short) + draft v0.2 as attachment; PGP or Signal
  for sensitive exchange.
- **Deliverable requested:** unstructured critique — prior art concerns,
  compatibility gotchas with GPL family, enforceability patterns observed
  in similar clauses elsewhere.
- **Output:** draft v0.3.

**Gate to Step 4:** at least two of three reviewers deliver substantive
feedback; divergent feedback reconciled explicitly.

---

## Step 4 — OSI submission decision (T+45 → T+60)

Binary decision: submit NPL-1.0 to OSI for approval, or remain
"source-available copyleft" without the OSI badge.

**Submit if:**
- Counsel (step 2) and peer reviewers (step 3) agree NPL-1.0 respects OSD.
- Strategic value of OSI badge (academic credibility, downstream adoption)
  outweighs the 3–6 month review cycle and the risk of OSI-mandated edits.

**Do not submit if:**
- OSI review would force changes that break §K4/§K5 design intent.
- Target audience (cosmon adopters) does not require the OSI label.

**Output:** short decision memo in `docs/license/osi-decision.md`.

---

## Step 5 — Repo switchover, Day J (mid-October 2026)

- Single commit migrating `LICENSE` from MPL-2.0 to NPL-1.0 (final text).
- Update `README.md` license section + badge.
- Update `CHANGELOG.md`: `docs(license): switchover MPL-2.0 → NPL-1.0
  (Day J)` with rationale and link to this plan.
- Run `cargo deny check licenses`. If `deny.toml` is strict-allowlist,
  add NPL-1.0 with its canonical URL to the allowlist.
- Existing contributor consent: if any external contributor has merged
  patches under MPL-2.0, obtain an individual waiver or a CLA amendment
  before flipping the top-level `LICENSE`.

**Gate to Step 6:** CI green on the switchover commit; `cargo deny`
passes; all contributors accounted for.

---

## Step 6 — Publish NPL-1.0 as a standalone text (post Day J)

- Publish the final NPL-1.0 text at a stable URL
  (`licenses.noogram.org/NPL-1.0` or equivalent).
- Provide a machine-readable metadata stub (SPDX-style `license-metadata.json`)
  with identifier, version, canonical URL, and OSI-status field.
- Make the license adoptable by third-party projects — this supports the
  wider *noogram = cognitive governance* narrative.

---

## Step 7 — Post-Day-J monitoring

- Track forks and redistributions of cosmon (GitHub search, manual audit).
  Verify in a sample whether §K4 attestation persistence is respected in
  practice.
- Collect violation patterns and edge cases in a private issue tracker
  (not as cosmon molecules — keep license enforcement out of the public
  backlog).
- If material ambiguities are detected, prepare NPL-1.1 as a minor
  revision; backport clarifications without breaking §K4/§K5/§K6 intent.

---

## Risks and contingencies

- **Counsel flags §K4 as unenforceable:** fallback is to reframe §K4 as a
  disclosure-only obligation (mirror §K5 structure) rather than a
  persistence obligation.
- **OSI rejects submission:** stay on "source-available copyleft"; do not
  relicense to a non-copyleft OSS license as a consolation.
- **Contributor consent cannot be obtained:** hold at MPL-2.0 until it
  is; do not flip unilaterally.
- **Legal review overruns T+30:** Day J slips; the plan has no hard
  deadline pressure that overrides legal soundness.

---

*End of validation plan.*
