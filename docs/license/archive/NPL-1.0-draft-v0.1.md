# Noogram Public License (NPL) — Version 1.0 (Draft v0.1) — REJECTED

> **STATUS: REJECTED, 2026-05-09 (ADR-092).**
>
> This draft is preserved for the historical record only. NPL-1.0 v0.1
> was an MPL-2.0 + §K4/§K5/§K6/§K7 superset that did not close the
> SaaS hole structurally — see ADR-092 §5 ("Why NPL-1.0 v0.1 is
> rejected"). The cosmon licence partition is now AGPL-3.0-only at the
> core (closes SaaS via §13) and Apache-2.0 at the frontier, aligned
> with noogram ADR-0001 §7 / ADR-0002 §5 / glossary / inversions §10.
>
> Do not adopt, cite, or extend this draft. Any future custom licence
> proposal must be a strict superset of AGPL-3.0 (never an MPL-derived
> weakening) and arrive via a successor ADR.
>
> See [`../../adr/092-license-bascule-mpl-to-agpl.md`](../../adr/092-license-bascule-mpl-to-agpl.md).

---

# Noogram Public License (NPL) — Version 1.0 (Draft v0.1)

> **Status:** INTERNAL DRAFT. Not yet reviewed by counsel. Do not redistribute
> outside the cosmon repository until at least v0.2 (post legal review).
>
> **Base:** Mozilla Public License, Version 2.0 (MPL-2.0), incorporated by
> reference.
>
> **Design intent:** neo-classical copyleft. Preserve MPL-2.0's file-level
> weak copyleft; add a *format-level* obligation for attestation artifacts
> (§K4), a symbolic disclosure rider for commercial redistribution (§K5),
> and a strict carve-out for private (non-distributed) use (§K6). Designed
> to stay inside the OSI-compatible perimeter — not SSPL/BUSL-style.

---

## Preamble

The Noogram Public License, Version 1.0 ("NPL-1.0"), extends the Mozilla
Public License, Version 2.0 ("MPL-2.0") with three clauses specific to
software that produces *auditable cognitive chains* — artifacts whose value
depends on the structural integrity of their provenance trail.

The additions are narrow:

- **§K4 — Attestation Persistence.** If the licensed software produces an
  Attestation Artifact (a structured record of a cognitive chain) and that
  artifact is redistributed or published, the *format* of the attestation
  must persist. The *content* remains the sole property of its producer.
- **§K5 — Disclosure Rider.** Commercial use that redistributes the licensed
  work (or derivative works) must publish a symbolic disclosure. The cost
  is €1 or equivalent — a legal signal, not a paywall.
- **§K6 — Private-Use Carve-out.** Local, internal, offline use triggers
  neither §K4 nor §K5. Solo experimentation, private research, and internal
  tooling remain fully unencumbered.

The license is designed so that viral obligations attach to *redistribution*
of attestation formats — not to the act of running the software, and not to
the content of the cognitive chains themselves.

---

## Body

Sections 1 through 11 of the Mozilla Public License, Version 2.0 are
incorporated herein verbatim by reference and form the body of this license.
The canonical text of MPL-2.0 is available at
<https://www.mozilla.org/MPL/2.0/>.

In case of conflict between MPL-2.0 §§1–11 and the additional sections §K4,
§K5, §K6, §K7 below, the additional sections govern only to the extent
strictly necessary to give effect to their narrow subject matter; all other
rights, obligations, and definitions of MPL-2.0 remain in full force.

---

## §K4 — Attestation Persistence

**K4.1 Definition.** An *Attestation Artifact* is any file produced by
execution of the Covered Software (as defined in MPL-2.0 §1.3) that records
an auditable cognitive chain. For the purposes of this license, a
cognitive chain is a structured sequence of at least two of the following
elements: (a) an operator prompt or intent statement; (b) a machine- or
human-produced briefing, plan, or frame; (c) step-by-step evolution records
(logs, events, per-step commits); (d) a synthesis, conclusion, or decision.

**K4.2 Persistence of Format.** If You redistribute or publish an
Attestation Artifact, or any derivative artifact that preserves, transforms,
or re-emits the cognitive chain it carries, You must preserve the
*structural format* of the attestation under NPL-1.0. The structural
format comprises: the identification of distinct chain elements, their
ordering, and the linkages between them. You are not required to preserve
any particular field names, file layout, or serialization — only the
recoverability of the chain structure by a reasonable auditor.

**K4.3 Content Ownership.** The *content* of an Attestation Artifact —
the substantive text, data, reasoning, or creative expression authored by
its producer — is not governed by §K4. It remains the sole intellectual
property of its producer and may be licensed, withheld, redacted, or
destroyed at that producer's discretion. §K4 attaches only to the
structural scaffolding that makes auditability possible.

**K4.4 Non-Covered Outputs.** Outputs produced by the Covered Software
that are not Attestation Artifacts (e.g., compiled binaries, data
transformations, or non-cognitive computations) are outside the scope of
§K4 and are governed solely by MPL-2.0.

---

## §K5 — Disclosure Rider

**K5.1 Commercial Use Trigger.** If You redistribute the Covered Software,
a Modification, or a Larger Work (as defined in MPL-2.0 §1) in the course
of *Commercial Use* — defined as sale, paid SaaS offering, paid derivative
licensing, or any distribution for direct monetary consideration — You
must publish a disclosure identifying (a) the use of software licensed
under NPL-1.0, (b) a stable link to the canonical text of this license,
and (c) the name or reference of the NPL-1.0 project(s) used.

**K5.2 Symbolic Cost.** The disclosure may take the form of a public web
page, a README section, or any equivalent stable publication. The cost
associated with publishing the disclosure is nominal: one euro (€1) or
equivalent commercial signaling cost. The intent of §K5 is to produce a
legal record of commercial usage, not to create a paywall or a licensing
fee.

**K5.3 Exemptions.** §K5 does not apply to: (a) private use as defined in
§K6; (b) non-commercial academic or scientific research; (c) internal use
by an organization where the Covered Software is not resold, offered as a
paid service, or incorporated into a revenue-bearing derivative.

---

## §K6 — Private-Use Carve-out

Use of the Covered Software that does not involve redistribution or
publication of the software, its Modifications, or its Attestation
Artifacts — including local use, internal use within a single legal
entity without external distribution, and offline experimentation — is
not subject to §K4 or §K5. Such private use is governed solely by the
rights granted in MPL-2.0.

This carve-out exists to preserve the freedom of individual
experimentation and of internal tooling within an organization. It does
not limit MPL-2.0's grant of rights; it narrows only the additional
obligations introduced by NPL-1.0.

---

## §K7 — Termination and Compatibility

**K7.1 Termination.** The rights granted under NPL-1.0 terminate
automatically if You fail to comply with §K4 or §K5, in the manner and
with the cure period described in MPL-2.0 §5. Rights granted by MPL-2.0
§§1–11 terminate according to MPL-2.0's own terms.

**K7.2 Compatibility with MPL-2.0.** Works licensed under NPL-1.0 may be
combined with works licensed under MPL-2.0 in the conditions set forth
in MPL-2.0 §3.3 (Larger Works). In such combinations, NPL-1.0 obligations
attach only to the files originally licensed under NPL-1.0 and to
Attestation Artifacts produced from them.

**K7.3 No Additional Restrictions.** Nothing in §K4, §K5, or §K6
restricts the rights of use, study, modification, or redistribution
granted by MPL-2.0 §2. The additional sections impose only a format
preservation obligation (§K4), a disclosure obligation for commercial
redistribution (§K5), and define the private-use carve-out (§K6).

---

## Definitions

Terms not defined below retain the meaning assigned to them in MPL-2.0 §1.

- **Attestation Artifact** — defined in §K4.1.
- **Cognitive Chain** — defined in §K4.1.
- **Commercial Use** — defined in §K5.1.
- **Covered Software** — as in MPL-2.0 §1.3.
- **Disclosure** — the public publication required by §K5.1.
- **Distributor** — any party who redistributes the Covered Software, a
  Modification, a Larger Work, or an Attestation Artifact.
- **Licensee / You** — any natural or legal person exercising rights
  under this license.
- **Private Use** — use described in §K6.
- **Structural Format** — defined in §K4.2.

---

*End of NPL-1.0 (Draft v0.1).*
