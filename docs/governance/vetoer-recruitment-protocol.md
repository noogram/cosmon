# Vetoer Recruitment Protocol

**Status:** v1, committed 2026-04-15.
**Parent deliberation:** [`delib-20260414-89dc`](../../.cosmon/state/fleets/default/molecules/delib-20260414-89dc/synthesis.md) — C11, D4 tranché.
**Supersedes:** [`vetoer-recruitment-v0.md`](vetoer-recruitment-v0.md) — the v0 gate (single vetoer by 2026-04-27) is preserved for historical continuity; this document is the canonical protocol for the Day-J horizon (J+180).
**Companion:** [`docs/editorial/publication-calendar-2026.md`](../editorial/publication-calendar-2026.md) — the public call fires on the same day as essay E3.

## Why this protocol exists

The cosmon system cannot be its own external witness. A Constitution that
ships without at least one adversarial reader outside the pilot's head is
indistinguishable from ship-theater: the invariants will be tuned to pass
the pilot's own blind spots (adversary panel §5; knuth axiom E; godel
convergence C10).

Recruiting vetoers is therefore the **minimum social implementation of
P_external**. This document defines *who* qualifies, *how* they are found,
*when* the public call fires, *how* a pre-enrollment bridge works without
poisoning the well, and *how* a vetoer is revoked, rotated, or succeeded.

It does **not** name candidates. Names are tracked in a separate private
document (not committed to the public repo) until the public call closes.

## Invariants (non-negotiable across amendments)

The following must hold under every revision of this protocol. They encode
the convergence of five panel personas (godin, hawking, turing, godel,
einstein) and the governance-continuity invariant from the parent synthesis.

1. **Criteria before names.** No vetoer is ever named before the public
   criteria and recruitment procedure are published (godin — "the first
   vetoer recruited in private poisons the well").
2. **Three-jurisdiction diversity.** The final vetoer set spans three
   distinct jurisdictions (e.g. one in the Americas, one in Europe, one
   elsewhere — the protocol does **not** prescribe which three). This
   resists single-state coercion and single-juridictional subpoena (hawking).
3. **Physical distinctness.** No two vetoers share a workstation, a
   household, or an employer. Threshold cryptography degenerates to 1-of-1
   when devices share a host (turing).
4. **Commercial independence.** No vetoer has a financial stake in the
   project, an employment or consulting relationship with the pilot or
   Noogram, or a contract whose renewal the pilot can control.
5. **Written ability to refuse.** Each vetoer provides prior evidence of
   having refused a proposal from someone they respect. Politeness is a
   disqualifying failure mode.
6. **Veto must be mechanically enforceable.** A vetoer who cannot block a
   merge, a signature, or a publication is not a vetoer. Veto authority
   is either a required reviewer on the relevant pull request, an
   out-of-band signature required to validate a release tag, or both.
7. **Operational confidentiality.** Identities remain private until the
   public call selection rationale is published. Keys, HW-token positions,
   and geographic coordinates of vetoers remain private forever.
8. **Channel separation.** Recruitment channel ⊥ proof channel ⊥
   operational channel (shannon). No single URL, no single email thread,
   no single messaging group links all three.
9. **Succession before recruitment.** The revocation, rotation, and
   replacement procedure (§6) is frozen in this document **before** any
   vetoer — pre-enrolled or public-call — is named. Succession cannot be
   invented reactively.
10. **Pre-enrollment is a bridge, not a terminus.** Vetoers enrolled
    privately before the public call have no special renewal rights; they
    re-apply through the public call like any other candidate.

## Public criteria

The following criteria are **publicly pre-published** in this document
before any name — public or pre-enrolled — is attached to the protocol.

### Hard criteria (all must hold)

Every vetoer, pre-enrolled or publicly selected, must satisfy *all* of:

1. **Seniority in formal methods.** Demonstrated via peer-reviewed
   publications, substantial OSS contributions to formal-verification tools,
   or a recognised track record in type theory, program verification,
   distributed-systems consensus, or applied cryptography.
2. **External to the pilot's daily work.** Not a co-author of any commit
   to the cosmon repository, not a member of Noogram, not in the
   pilot's default agent panel (wheeler, torvalds, popper, …), not a
   regular collaborator on any other pilot-led project.
3. **Jurisdictional independence.** Legal residence in a jurisdiction
   distinct from the pilot's and from each other vetoer's. The final set
   must span three distinct jurisdictions; individual applications specify
   residence explicitly.
4. **Commercial independence.** No current or past employment or
   consulting relationship with the pilot or Noogram. No financial stake.
   No contract whose renewal the pilot can control.
5. **Willingness to resist coercion.** Prior evidence of refusing a
   proposal from someone they respect — publicly visible (blog post, public
   review, GitHub thread) or verifiable by the pilot via a third party.
6. **Minimum time budget.** Commit to ≥2 hours per amendment of the
   Constitution or its enforcing CI checks, with a documented presence
   cadence (response within 14 calendar days to any veto-requesting event).
7. **Ability to read the TCB.** Capable of reading ≤500 lines of Rust or
   the relevant language of the TCB, and ≤200 lines of CI configuration,
   and judging whether an invariant is decidable and whether its check
   actually enforces it.

### Soft criteria (weighted favourably, not required)

- Prior public technical writing.
- Familiarity with Rust or an explicit willingness to read `cargo test`
  output.
- Experience with threshold signatures or HW-token ceremonies.
- A track record of shipping governance documents (bylaws, code of conduct,
  security policy) that produced measurable behavioural change.

### Exclusions (hard no)

- Anyone with direct or indirect commercial interest in a competing or
  aligned agent-orchestration project that would be affected by the
  Constitution.
- Anyone who has publicly advocated for the pilot on terms that would
  make a veto socially costly (politeness failure mode).
- Anyone who refuses to sign their vetoes. Anonymous vetoes undermine the
  external-witness property.

## Pre-enrollment (P1 bridge, private)

### Purpose

The window between J+0 and the public call (essay E3, 2026-06-09) is ~60
days. During this window, the project needs *some* adversarial reader for
CONSTITUTION v0 draft work, key-ceremony design, and P0/P1 decisions. The
pre-enrollment bridges that gap without degenerating into permanent
private cooptation.

### Process

1. **Slots.** At most **two** pre-enrolled vetoers. Not three, not one.
   Two is the minimum that avoids bus-factor collapse during the bridge
   window; three would make the pre-enrollment itself the committee and
   defeat the public call.
2. **Invitation form.** Written outreach only (email or long-form
   messaging), one candidate at a time, with:
   - A pointer to this protocol document.
   - An explicit statement that the enrollment is a **bridge**, not a
     permanent seat; the candidate must re-apply through the public call.
   - The criteria above, verbatim, with a request for written
     acknowledgement that the candidate meets them.
   - The estimated time commitment (≥2h per amendment) and the
     non-monetary nature of the role.
3. **Acceptance.** Written acceptance from the candidate, archived in a
   private location (encrypted or in a non-public remote). Acceptance must
   reference this protocol by its OTS-anchored hash at the time of writing.
4. **Duration.** Pre-enrollment expires **automatically** on the date the
   public call selection rationale is published (target: essay E5,
   2026-08-11). No extension possible. Silent continuation is a protocol
   violation.
5. **Public disclosure.** The **fact** that pre-enrollment exists, and the
   number of pre-enrolled vetoers, are published in this document and in
   essay E3 (which opens the public call). The **identities** of the
   pre-enrolled are published nominatively **in this document before the
   public call opens**, not after. This is the well-poisoning mitigation:
   applicants to the public call know who else is in the room.
6. **No renewal of right.** A pre-enrolled vetoer who wishes to become a
   permanent vetoer must re-apply through the public call, on the same
   terms as any other applicant. The selection committee (§4.3 below)
   evaluates them on their application materials, not on their
   pre-enrollment history.

### What pre-enrolled vetoers can veto during the bridge

- Draft Constitution text and its CI checks.
- Key-ceremony design for P2.
- Decisions that would materially change the TCB surface.

### What they cannot do

- Speak on behalf of the project publicly.
- Be named as vetoers in essays before the public call (E3 is the first
  essay that names the number of pre-enrolled; E5 is the first that
  publishes the public selection rationale).
- Extend their own mandate.

## Public call (fires at essay E3, 2026-06-09)

### Trigger

The public call opens **on the same day** essay E3 is published. The
essay's final section announces the opening and links to this document.

### Application window

- **Open:** 2026-06-09 (day of essay E3).
- **Close:** 2026-07-09 (30 days).
- **Extensions:** none; a second window opens only if the selection
  committee cannot assemble a final set meeting the three-jurisdiction
  invariant (§2).

### Application materials

Each applicant submits:

1. A one-page cover letter addressing the hard criteria above. No CV
   padding; specific claims only.
2. Two public artefacts (papers, OSS commits, blog posts) that
   demonstrate the formal-methods seniority criterion.
3. One public artefact demonstrating the willingness-to-refuse criterion
   (criterion 5).
4. A written acknowledgement that they meet the commercial-independence
   criterion and that they will disclose any change in that status.
5. An explicit statement of jurisdiction of legal residence.
6. A PGP or SSH public key, or an Ed25519 HW-token public key, that will
   be used for future signed vetoes.

### Selection committee

The selection committee consists of:

- The pilot (Noogram).
- The pre-enrolled vetoer(s) from the bridge (at most two).

Decisions require **unanimity** among the committee members on each
candidate. A tied or split vote is a rejection.

### Selection rationale

For each selected vetoer, the committee writes a **public rationale**
explaining why the candidate was selected, how they satisfy the hard
criteria, and why their jurisdiction complements the final set. The
rationale **does not disclose**:

- The vetoer's residential address.
- The HW-token fingerprint or any key material.
- Internal communications with the candidate during the selection.

The rationale **does disclose**:

- The vetoer's name, with their written consent.
- The jurisdiction of legal residence (country only).
- A summary of which public artefacts satisfied which criteria.

The rationale is published in essay E5 (2026-08-11). If E5 must be a
cadence-note (see the publication calendar), the rationale slips to E6
but not further; a second slip triggers a Constitution-collapse review.

### Target set size

The final set contains **three** vetoers meeting the three-jurisdiction
invariant. Fewer than three is a protocol violation. More than three is
allowed only by explicit amendment of this document.

## Succession clause

### Revocation

A vetoer is revoked when **any** of the following holds:

- The vetoer writes a signed resignation.
- A majority of the remaining vetoers votes for revocation (two of two,
  or two of three), in writing, with rationale.
- The vetoer acquires a commercial interest that violates criterion 4;
  failure to self-disclose within 30 days of the change is grounds for
  automatic revocation.
- The vetoer has failed to respond to two consecutive veto-requesting
  events within the 14-day cadence.

Revocation is published: name of revoked vetoer, rationale, date, OTS-
anchored.

### Rotation

Any vetoer may request rotation off the committee at any time by written
notice with ≥30 days' transition. During the transition, the departing
vetoer remains active for all in-flight Constitution amendments but
participates in no new ones. A replacement is recruited via the public
call procedure (§4); no private cooptation is permitted even for
replacements.

### Dead-man trigger (pilot unreachable)

If the pilot is unreachable for **90 consecutive days** (no signed commit,
no response to a signed challenge sent by any vetoer), the vetoers can
collectively trigger the **succession protocol**:

1. Two of three (or two of two during the bridge) vetoers sign a
   written succession notice.
2. The notice opens a sealed envelope (physically held by one pre-
   designated third party, not a vetoer) containing:
   - Dormant recovery material for the repository.
   - A named successor pilot (a human, not an institution) who has
     signed, before the dead-man trigger was armed, a written
     acceptance of the role and of this protocol.
3. The successor pilot takes over with the full set of vetoers intact.
4. The trigger event is chronicled and OTS-anchored.

The third-party envelope-holder is **not** a vetoer, has no veto
authority, and holds no signing keys — they hold physical access to
the sealed envelope only. They are named in a separate private document
and succeed by the same pre-designation mechanism.

### Forced replacement under coercion

If the pilot (or a vetoer) detects that a vetoer has been coerced —
rubber-hose extraction, legal subpoena the vetoer cannot refuse, physical
threat — the detecting party triggers an **emergency rotation**:

1. A signed notice is sent to the remaining vetoers through the
   operational channel.
2. The coerced vetoer's signing key is revoked from the release
   verification path immediately.
3. A replacement is recruited via an accelerated public call (14-day
   window instead of 30) or, if the coercion demands a bridge, via a
   one-slot pre-enrollment with automatic expiry at the next full public
   call.
4. The coercion event is chronicled; the details of what was coerced
   remain private.

## Confidentiality operational rules

1. **Identities.** Public until the public call selection rationale is
   published (E5). Before that, only the **number** of pre-enrolled
   vetoers is public. After, identities are public but residential
   addresses, employers, and HW-token positions remain private.
2. **Keys.** Public keys published with each vetoer's name post-selection.
   Private keys are HW-bound (Yubikey or equivalent) and never leave the
   hardware. No software-bound signing.
3. **Geographic detail.** Country of legal residence is public. City,
   region, employer, affiliations beyond country are **not** public.
4. **Communication channels.**
   - **Recruitment channel:** email, written, archivable. Used only during
     outreach and application intake.
   - **Operational channel:** Signal (or equivalent end-to-end encrypted
     messenger), one group per generation of vetoers. Used for veto
     requests, key-ceremony coordination, succession triggers.
   - **Proof channel:** signed commits, signed release tags, OTS anchors.
     Used for the publicly verifiable veto outcome.

   These three channels must not share any identifiers (no shared URL, no
   shared handle, no cross-linked accounts).
5. **Co-presence.** Vetoers do not co-attend conferences, workshops, or
   meetings where all three (or two of two) are physically present, until
   Day J is past. The attack surface of a co-presence event during the
   90-day window is unnecessary.

## What this protocol does not do

- **It does not name candidates.** That list is tracked privately.
- **It does not prescribe specific jurisdictions.** Only that three must
  be distinct.
- **It does not specify the signing scheme cryptographically.** See the
  crypto-signatures research task (child C of the parent deliberation)
  for the 2-of-3 vs FROST decision.
- **It does not define the Constitution's content.** See the Constitution
  kernel task (child D of the parent deliberation).
- **It does not guarantee the vetoers will be competent or honest.** It
  raises the cost for an adversary and restricts the failure modes the
  pilot can cause alone. The external-witness property is statistical,
  not absolute.

## Amendment rule

Amendments to this protocol require:

1. A written proposal, OTS-anchored before discussion begins.
2. Unanimous approval by the current vetoer set (no split vote).
3. A 14-day public comment window on the permission list.
4. Republication of this document with a new version stamp and a new OTS
   anchor.

Silent changes are a protocol violation; the chronicle must flag them.
