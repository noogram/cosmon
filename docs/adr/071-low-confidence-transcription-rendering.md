# ADR-071 — Low-confidence transcription body rendering (§8o)

**Status:** Proposed
**Date:** 2026-04-23
**Parent:** `delib-20260422-9059` (low-confidence signaling deliberation)
**Grandparent:** `delib-20260422-6e2c` (voice-messages deliberation — D2
deferred this question)
**Panel:** JR, Einstein, Shannon, Feynman
**Supersedes:** none
**Follow-up:** (to be nucleated as `temp:warm`) `render(segments,
threshold) → body` function implementation in `cosmon-matrix-tick` STT
enrichment pass; test harness asserting the projection property.

## Context

Parent deliberation `delib-20260422-6e2c` (voice messages, seven
personas) deferred one UX question. When a transcribed whisper contains
one or more segments whose confidence (`avg_logprob` in the current
OpenAI Whisper schema) falls below a policy threshold, how is that
uncertainty rendered to the human reader of the whisper markdown file?

Three candidate rules surfaced in the parent delib (parent synthesis
§D2):

| # | Source | Rule | Example body |
|---|---|---|---|
| **A** | JR | No visible marker. Body reads as if typed; uncertainty lives **only** in frontmatter `transcription_segments[i].avg_logprob`. | `Hello world ship it` |
| **B** | Einstein | Single `[?]` body head-prefix if any segment falls below threshold. One bit, loud. | `[?] Hello world ship it` |
| **C** | Shannon | Per-segment inline markup `[?low-conf: <span>]` wrapping uncertain spans. | `Hello world [?low-conf: ship it]` |

The parent deliberation deferred the verdict for two reasons:

1. The v0 Torvalds option (untranscribed-only path, `authored_via:
   untranscribed`) does not encounter the problem — no transcript, no
   confidence to render. The rule only applies to Tier 3.
2. The resolution requires trial against real voice samples
   (EN + FR, the operator's working languages), not a pure armchair
   verdict.

This deliberation (`delib-20260422-9059`) resolved the question with a
four-persona panel (JR + Einstein + Shannon from the parent tension,
plus Feynman as tie-breaker and naive-reader auditor). All four wrote
trial renders on simulated noisy transcription segments; all four
proposed a named invariant rule suitable for inscription.

## Decision

Inscribe new invariant **§8o — Low-confidence transcription rendering**
as a peer of `docs/architectural-invariants.md §8n`
(Voice Provenance Closure) and orthogonal to §8k/§8k'
(cross-surface wheat-paste). The canonical body of a transcribed
whisper SHALL wrap each contiguous run of below-threshold tokens in
plain-ASCII square brackets of the form `[?<span-text>]` (no
whitespace padding, no label). The body SHALL be a deterministic
projection of `transcription_segments` × threshold, asserted in tests
by `render(segments, threshold) == body`.

### Clause text (to be inscribed into `docs/architectural-invariants.md`)

> **§8o. Low-confidence transcription rendering** *(proposed —
> `delib-20260422-9059`, ADR-071)*.
>
> When a whisper's `authored_via` is `dictated` or `transcribed`,
> every contiguous run of tokens whose source
> `transcription_segments` entries fall below the transcription
> model's configured low-confidence threshold MUST be wrapped in
> the body as `[?<span-text>]` — opening square bracket, question
> mark, span text verbatim, closing square bracket, no whitespace
> padding and no label. Confident spans render unwrapped. The wrap
> is contiguous-minimal: adjacent sub-threshold segments collapse
> into a single wrap; single uncertain tokens wrap alone when the
> schema provides token-level granularity.
>
> The body is therefore a **deterministic projection** of
> `transcription_segments` under a named threshold policy. A
> render function `render(segments, threshold) → body` MUST be
> reproducible and asserted in tests; the two MUST match
> byte-for-byte for every emitted whisper.
>
> The threshold value and its per-language / per-model calibration
> live with the transcription model configuration alongside
> `transcription_model@version`, **not** in this invariant. §8o
> fixes the shape of the hedge, not the firing rule.
>
> §8o is orthogonal to `docs/architectural-invariants.md §8k` and
> §8k'. §8o governs what the canonical body bytes on disk contain;
> §8k governs how those bytes render across surfaces. A `[?…]`
> wrap is a plain-text ASCII glyph — written once at enrichment,
> byte-identical across every viewport, grep-addressable on disk —
> not a per-surface affordance. `docs/architectural-invariants.md §8n`
> (Voice Provenance Closure) remains the audit chain; §8o is the
> body-surface hedge that makes the closure actionable to the
> naive reader.

### Trial renders — canonical body bytes under §8o

**EN sample** (segments 1–2 confident, segment 3 at avg_logprob
`-1.12` below a `-0.9` working-placeholder threshold):

```
Hello world ship it [?by end of quarter]
```

**FR sample** (segment 2 — proper noun "Tenant-Demo" — at avg_logprob
`-1.45`; adjacent segments confident):

```
On valide avec [?Tenant-Demo] la semaine prochaine
```

## Alternatives considered

### Rule A — no body glyph, viewport tints the row (JR)

**Strongest argument for.** Preserves the wheat-paste principle
absolutely — a dictated whisper and a typed whisper are the same
face at the eye on disk. The amber-wash tint is a legitimate
viewport affordance already authorised by
`docs/architectural-invariants.md §8k'` (*A viewport MAY … tint for
dark / light*). `cs peek --snapshot` emits a background-colour byte
on rows whose frontmatter predicate fires; every surface inherits
the tint via `WheatPasteView`.

**Strongest argument against.** Fails the **grep-on-disk test**. A
verifier (`cs verify whispers` sweep, external export tool, the
operator catting a file) cannot detect low-confidence content from
the body bytes alone. Feynman's walkthrough quantified this: under
Rule A, a verifier must write a YAML parser *plus* a segment-to-body
offset aligner (the segment `start`/`end` timestamps do not map to
body character offsets). Under Rule C, a verifier writes a one-line
`grep '\[\?[^]]*\]'`. Furthermore, Rule A reduces §8n to a latent
guarantee with no observable trigger — the operator scrolling
whispers on a train has no mechanism by which to notice a
hallucination, so the §8n audit chain is walkable *in principle* but
unwalked *in practice*, which Feynman showed is indistinguishable
from a body that tells the truth.

### Rule B — single `[?]` head-prefix (Einstein)

**Strongest argument for.** Minimum body mutation — three bytes at
the head, never more. Satisfies the grep-on-disk test and answers
the falsifiability objection with a single observable bit. Dual-
channel rendering: summary bit on skim channel, full vector on
forensic channel (frontmatter, §8n).

**Strongest argument against.** Alerts without localizing. Feynman's
walkthrough: in a 30-segment whisper with one out-of-distribution
proper noun, `[?]` at the head tells the operator "something is off"
— the operator has no choice but to re-listen or open YAML, which
defeats the skim channel entirely. Shannon quantified: Rule B
carries ≈1/N bits per segment with −log₂(N) bits of localization
debt. Per-span markup lets the operator decide in one second whether
the flagged span matters (low-confidence on "by end of quarter" vs.
on "Tenant-Demo" is a very different decision) without leaving the
skim channel.

### Rule C — per-span inline markup with `⟨…⟩` (Shannon's refinement)

**Strongest argument for.** Densest well-formed encoding, 6 UTF-8
chrome bytes per span, orphan in operator prose (no collision with
Markdown emphasis, GFM strikethrough, or punctuation). Visually
asymmetric and mathematical, processed as margin annotation by the
reader's eye.

**Strongest argument against.** Requires UTF-8 and specific glyph
support. Some monospace fonts render U+27E8/U+27E9 as missing-glyph
boxes; some POSIX regex engines handle Unicode angle brackets
poorly. Not consistent with Tier 2's existing `[audio · 32s ·
mxc://…]` placeholder convention, which establishes square brackets
as the Tier-system's modality-specific body-glyph family. ASCII
portability dominates density on a data-plane artifact.

## Rationale

Four personas reviewed the question against first principles, the
three candidate rules, and trial renders on EN + FR samples. The
synthesis converged on Rule C with Feynman's sharpening
(`[?<span>]` ASCII form, no threshold inscription, no granularity
commitment, body-as-projection semantics). Vote tally:

| Rule family | Endorsements |
|---|---|
| A (no body glyph) | JR (1) |
| B (single head-prefix) | Einstein (1) |
| C-family (per-span wrap) | Shannon + Feynman (2) |

The decisive arguments:

1. **Tier 2 precedent.** JR himself authored the Tier 2
   `[audio · 32s · mxc://…]` placeholder in the parent delib. That
   placeholder is a body glyph a typed whisper cannot produce, and
   it is accepted by every persona (including JR). The wheat-paste
   principle — *a dictated whisper and a typed whisper must be the
   same face at the eye* — is therefore already a conditional
   principle: it holds within a modality, not across modalities.
   Tier 3 transcribed whispers with low-confidence spans are a
   different condition than Tier 2 untranscribed blobs, and merit
   the same modality-specific dispensation by the same logic.

2. **Grep-on-disk test.** Einstein's three-clause admissibility
   test for body glyphs — *written once at origin, renders
   byte-identically across every viewport, grep-addressable on
   disk* — is satisfied by `[?<span>]` and not satisfied by JR's
   amber-wash tint. The test captures why Tier 2's placeholder is
   legitimate: the bytes are on disk, the renderers agree, the
   verifier can grep. `[?<span>]` satisfies the same three
   clauses with the same logic.

3. **Feynman's "triage without diagnosis."** Einstein's one-bit
   argument is right about the *form* of the skim receiver's
   decision (a yes/no) but wrong about the *information* the
   receiver needs to form it. The locus of uncertainty is load-
   bearing for the skim decision, not just for the forensic
   decision. A `[?]` at the head forces every flagged whisper
   into a YAML read or audio re-listen; a per-span wrap resolves
   most of them at a glance.

4. **§8o is orthogonal to §8k.** §8k governs how viewports
   re-render the same canonical state; §8o governs what the
   canonical state contains. Stacking them conflates the charter.
   The ADR and the invariant explicitly note the orthogonality;
   no future modification to §8o can compromise §8k and vice
   versa. A viewport MAY additionally tint a row whose body
   already contains `[?…]` markers (JR's amber-wash proposal) as
   auxiliary signal — no conflict.

### Feynman's three non-negotiables (inscribed in the clause text)

The tie-breaker's hidden-assumption audit identified three failure
modes the other personas assumed away. Each is addressed in the
clause:

1. **No threshold inscribed.** The invariant names the shape of the
   hedge, not the firing rule. `avg_logprob` distributions shift
   with language (FR runs more negative than EN at the tokenizer
   level), microphone / SNR, domain (proper nouns and code
   identifiers always score low), and model version. A threshold
   baked into the invariant ages into a fossil within two model
   releases. The threshold is a policy knob, colocated with
   `transcription_model@version`.

2. **No granularity commitment.** The clause uses "contiguous run of
   tokens" and "segments … when the schema provides token-level
   granularity" without pre-committing to segment-vs-token wrap.
   Current Whisper schema provides segment-level `avg_logprob`; a
   future schema extension with per-token logprobs can tighten the
   wrap without an invariant amendment.

3. **Body as projection.** `render(segments, threshold) == body`
   must hold byte-for-byte and be asserted in tests. This preserves
   §8n closure by construction — the body remains a deterministic
   view of the auditable record, never an independent authoring
   decision — and leaves room for read-time rendering (the surface
   picks a threshold and re-projects) without a successor
   invariant.

## Implementation notes (non-normative)

Out of scope for this ADR. Deferred to a follow-up `temp:warm`
task that this deliberation's step 4 will nucleate:

- The `render(segments, threshold) → body` function lands in
  `cosmon-matrix-tick` (or a successor enrichment crate) alongside
  the STT enrichment pass.
- The projection property is asserted as a property test
  (governance-tier *Stable* or *Production* per
  `docs/architectural-invariants.md` testing policy).
- The threshold policy lives in `.cosmon/config.toml` under a
  `[whispers.transcription]` table, with per-language overrides.
- Existing Tier 2 writer (`cosmon-matrix-tick`) is unchanged; §8o
  applies only to Tier 3 (`authored_via ∈ {dictated,
  transcribed}`), which the current untranscribed-v0 writer does
  not emit.

## Consequences

### Positive

- Low-confidence spans are visible to the naive reader without
  opening the drawer — §8n becomes actionable, not merely
  auditable.
- Verifier and exporter tools get a one-line `grep` for
  low-confidence detection, replacing schema-aware YAML walks.
- §8k and §8k' remain intact; no surface-specific rendering
  affordance is introduced.
- The invariant survives model-version and schema evolution
  (no threshold or granularity baked in).

### Negative

- Tier 3 transcribed whisper bodies are no longer byte-identical
  to typed whispers with the same text. This is an explicit
  category split (acknowledged: "a typed whisper and a dictated
  whisper are not the same speech act" — Feynman), not a leak.
- Any external consumer that renders whisper bodies as prose
  without honouring `[?…]` markers shows the markers as raw
  characters. Mitigation: the markers are visually familiar
  (compare `[sic]`, `[unclear]`) and semantically parseable
  without special handling.
- The `render` function is a new piece of infrastructure that
  must be tested and maintained. Cost is bounded (a handful of
  pattern-matching cases); the property-based test closes the
  correctness surface.

### Open questions (for the follow-up task, not for this ADR)

- What are the per-language threshold defaults (EN, FR, multi-
  lingual)? Feynman flagged that a universal `-0.9` is
  language-biased.
- How does the writer handle the `transcription_segments: []`
  (empty-segments fallback) case? Current answer: no wraps
  fire, body is the concatenated-text fallback the frontmatter
  schema already defines.
- If a future schema adds per-token logprobs, how do adjacent
  token-level wraps collapse into a single bracket? Current
  answer: the "contiguous-minimal" clause already covers it —
  adjacent sub-threshold tokens merge into one wrap.

## References

- `docs/architectural-invariants.md §8k` — wheat-paste invariant
  (ADR-064 §C4, *postman's uniform stays outside the house*).
- `docs/architectural-invariants.md §8k'` (L1218+) — cross-surface
  wheat-paste (ADR-066).
- `docs/architectural-invariants.md §8n` (L1617+) — Voice
  Provenance Closure (`delib-20260422-6e2c`).
- `docs/whisper-frontmatter-schema.md` — canonical frontmatter,
  three density tiers, Tier 2
  `[audio · 32s · mxc://…]` placeholder precedent.
- `docs/adr/066-ux-v2-substrate.md` — §8k' substrate,
  `WheatPasteView` primitive.
- `.cosmon/state/fleets/default/molecules/delib-20260422-6e2c/synthesis.md §D2`
  — parent deferral.
- `.cosmon/state/fleets/default/molecules/delib-20260422-9059/synthesis.md`
  — this deliberation's integrated synthesis.
- `.cosmon/state/fleets/default/molecules/delib-20260422-9059/responses/{jr,einstein,shannon,feynman}.md`
  — panel responses.
